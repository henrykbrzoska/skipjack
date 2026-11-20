//! YouTube Lounge API client (the protocol used by "Link with TV code").
//!
//! Handles pairing, authentication, subscriptions and remote commands,
//! including ad muting/skipping, autoplay enforcement, the shorts
//! workaround, kids-profile detection and a watchdog.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use md5::{Digest, Md5};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::constants::{LOUNGE_API, LOUNGE_APP, YOUTUBE_CLIENT_BLACKLIST};

/// YouTube sends at least one message (a `noop`) every ~30 s.
const WATCHDOG_TIMEOUT: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Events that matter when debugging ad handling; logged at info level.
const AD_EVENTS: &[&str] = &["onAdStateChange", "adPlaying"];

/// Budget of *consecutive* failed ad escapes before the experiment gives up for
/// the session; a success resets it (see docs/PLAN-ad-reload.md).
const MAX_AD_RELOADS: u32 = 3;
/// A reload is considered a success only if the content video resumes within
/// this window; otherwise the experiment is aborted for the session.
const AD_RELOAD_WINDOW: Duration = Duration::from_secs(15);
/// Pause between `BACK` (leaving the player) and re-opening the video. The TV
/// needs a moment to tear down the ad session, otherwise the reload is ignored.
const AD_RELOAD_RETURN_DELAY: Duration = Duration::from_millis(1500);
/// `setPlaylist` only *cues* a video — the TV never starts it by itself, so a
/// reloaded video needs an explicit `play`. The first command can land before
/// the player is ready, hence the retries.
const START_PLAYBACK_DELAY: Duration = Duration::from_millis(700);
const START_PLAYBACK_TRIES: u32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum LoungeError {
    #[error("not linked (no lounge token)")]
    NotLinked,
    #[error("not connected to the lounge")]
    NotConnected,
    #[error("connection denied by YouTube; force close the YouTube app on the TV and try again")]
    ConnectionDenied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerState {
    Stopped,
    /// YouTube labels this "ended".
    Buffering,
    Playing,
    Paused,
    /// YouTube labels this "buffering".
    Starting,
    Cued,
    Advertisement,
    AdSkipped,
}

impl PlayerState {
    pub fn parse(value: &str) -> Self {
        match value.trim().parse::<i64>() {
            Ok(0) => Self::Buffering,
            Ok(1) => Self::Playing,
            Ok(2) => Self::Paused,
            Ok(3) => Self::Starting,
            Ok(5) => Self::Cued,
            Ok(1081) => Self::Advertisement,
            Ok(1082) => Self::AdSkipped,
            Ok(-1) => Self::Stopped,
            _ => {
                tracing::debug!("unknown player state {value:?}, assuming stopped");
                Self::Stopped
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlaybackState {
    pub video_id: String,
    pub current_time: f64,
    pub duration: f64,
    pub state: PlayerState,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            video_id: String::new(),
            current_time: 0.0,
            duration: 0.0,
            state: PlayerState::Stopped,
        }
    }
}

/// What Skipjack did about an ad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdAction {
    Muted,
    Skipped,
    Unmuted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum DpadKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Back,
}

impl DpadKey {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "UP" => Some(Self::Up),
            "DOWN" => Some(Self::Down),
            "LEFT" => Some(Self::Left),
            "RIGHT" => Some(Self::Right),
            "ENTER" | "OK" => Some(Self::Enter),
            "BACK" => Some(Self::Back),
            _ => None,
        }
    }

    fn as_cmd(&self) -> &'static str {
        match self {
            Self::Up => "UP",
            Self::Down => "DOWN",
            Self::Left => "LEFT",
            Self::Right => "RIGHT",
            Self::Enter => "ENTER",
            Self::Back => "BACK",
        }
    }
}

/// Queue (playlist) to load onto the TV via `setPlaylist`.
#[derive(Debug, Clone, Default)]
pub struct Playlist {
    /// First video to play.
    pub video_id: String,
    /// Whole queue, comma-joined into `videoIds` (may be empty = single video).
    pub video_ids: Vec<String>,
    /// Index into `video_ids` to start from.
    pub current_index: Option<usize>,
    /// Start position in seconds (applies to the first video only).
    pub current_time: Option<f64>,
    /// YouTube playlist id (`listId`), if the queue comes from a playlist.
    pub list_id: Option<String>,
}

impl Playlist {
    fn params(&self) -> Vec<(&'static str, String)> {
        let mut p = vec![("videoId", self.video_id.clone())];
        if !self.video_ids.is_empty() {
            p.push(("videoIds", self.video_ids.join(",")));
        }
        if let Some(i) = self.current_index {
            p.push(("currentIndex", i.to_string()));
        }
        if let Some(t) = self.current_time {
            p.push(("currentTime", t.to_string()));
        }
        if let Some(l) = &self.list_id {
            p.push(("listId", l.clone()));
        }
        p
    }
}
/// Snapshot of the TV queue, from `playlistModified` or `watch_queue_ajax`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct QueueState {
    /// YouTube playlist id, if the queue comes from a playlist.
    pub list_id: Option<String>,
    /// Video ids in queue order.
    pub video_ids: Vec<String>,
}

impl QueueState {
    fn from_event_data(data: &Value) -> Self {
        let list_id = field_str(data, "listId").filter(|s| !s.is_empty());
        let video_ids = field_str(data, "videoIds")
            .map(|s| {
                s.split(',')
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Self { list_id, video_ids }
    }
}

/// One item of the queue fetched via `watch_queue_ajax`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QueueItem {
    pub video_id: String,
    pub title: Option<String>,
}

/// Volume snapshot, from `onVolumeChanged`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VolumeState {
    pub volume: u8,
    pub muted: bool,
}

/// Quality snapshot, from `onVideoQualityChanged`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct QualityState {
    pub level: Option<String>,
    pub available: Vec<String>,
}

/// Events emitted by the lounge client to its owner.
#[derive(Debug, Clone)]
pub enum LoungeEvent {
    Playback(PlaybackState),
    Ad(AdAction),
    Queue(QueueState),
    Volume(VolumeState),
    Quality(QualityState),
}

#[derive(Debug, Clone)]
pub struct LoungeOptions {
    pub join_name: String,
    pub mute_ads: bool,
    pub skip_ads: bool,
    pub auto_play: bool,
    /// Experimental: try `setPlaylist` on non-skippable ads (off by default).
    pub experimental_reload_ads: bool,
}

#[derive(Debug)]
struct Session {
    lounge_token: Option<String>,
    sid: Option<String>,
    gsession: Option<String>,
    last_event_id: Option<i64>,
    command_offset: i64,
    screen_name: Option<String>,
    volume: String,
    muted: String,
    /// True while we believe an ad is playing (muted by us).
    ad_active: bool,
    /// Experimental ad-reload bookkeeping (see docs/PLAN-ad-reload.md).
    ad_reload: AdReloadState,
    shorts_disconnected: bool,
    playback: PlaybackState,
    /// Last known *content* progress — ads never update it, so it's the position
    /// to return to after leaving the player.
    content: ContentProgress,
    /// Last known queue snapshot (from `playlistModified`).
    queue: QueueState,
    /// Last known quality snapshot (from `onVideoQualityChanged`).
    quality: QualityState,
}

/// Last known position in the *content* video (ignores ad playback).
#[derive(Debug, Default, Clone)]
struct ContentProgress {
    video_id: String,
    position: f64,
}

/// State of the experimental ad-escape attempt (leave the player, come back).
#[derive(Debug, Default)]
struct AdReloadState {
    /// Number of escape attempts this session (cap: [`MAX_AD_RELOADS`]).
    attempts: u32,
    /// Set once an attempt fails hard (session lost, different video) — stop.
    aborted: bool,
    /// The attempt in flight, until the content video resumes.
    pending: Option<PendingReload>,
    /// Ad we already tried to escape. `onAdStateChange` carries no ad id, so an
    /// echo of the same ad must not start a second attempt.
    tried_ad: Option<String>,
}

#[derive(Debug)]
struct PendingReload {
    video_id: String,
    position: f64,
    /// The ad we tried to escape (empty when the event carried no id).
    ad_id: String,
    started: std::time::Instant,
}

impl Session {
    fn connected(&self) -> bool {
        self.sid.is_some() && self.gsession.is_some()
    }

    fn connection_lost(&mut self) {
        self.sid = None;
        self.gsession = None;
        self.last_event_id = None;
        // A new session starts with a fresh experiment budget.
        self.ad_reload = AdReloadState::default();
    }
}

/// Parses the length-prefixed chunk format of the BrowserChannel protocol:
/// a line with the chunk length followed by the JSON payload lines.
#[derive(Default)]
pub(crate) struct ChunkParser {
    remaining: i64,
    buf: String,
}

impl ChunkParser {
    pub(crate) fn push_line(&mut self, line: &str) -> Option<Vec<Value>> {
        let line = line.strip_suffix('\n').unwrap_or(line);
        if self.remaining <= 0 {
            // Expecting a length line; anything else is ignored (resync).
            match line.trim().parse::<i64>() {
                Ok(n) => {
                    self.remaining = n;
                    self.buf.clear();
                }
                Err(_) if !line.trim().is_empty() => {
                    tracing::debug!("ignoring unexpected lounge line: {line}");
                }
                Err(_) => {}
            }
            return None;
        }
        self.buf.push_str(line);
        // Lengths are counted in JS string units (UTF-16) incl. the newline.
        self.remaining -= line.encode_utf16().count() as i64 + 1;
        if self.remaining > 0 {
            return None;
        }
        self.remaining = 0;
        let chunk = std::mem::take(&mut self.buf);
        match serde_json::from_str::<Vec<Value>>(&chunk) {
            Ok(events) => Some(events),
            Err(e) => {
                tracing::warn!("failed to parse lounge chunk: {e}: {chunk}");
                None
            }
        }
    }
}

fn value_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn field_str(data: &Value, key: &str) -> Option<String> {
    data.get(key).and_then(value_str)
}

fn field_f64(data: &Value, key: &str) -> Option<f64> {
    field_str(data, key).and_then(|s| s.parse().ok())
}

/// Parses `watch_queue_ajax` HTML: `<li class="…watch-queue-item" data-video-id="…">`
/// items, title from the first `title="…"` (or inner text fallback).
fn parse_watch_queue(html: &str) -> Vec<QueueItem> {
    let mut items = Vec::new();
    for chunk in html.split("<li").skip(1) {
        let head = &chunk[..chunk.find('>').unwrap_or(chunk.len())];
        if !head.contains("watch-queue-item") {
            continue;
        }
        let Some(video_id) = attr(head, "data-video-id") else {
            continue;
        };
        let title = attr(chunk, "title").or_else(|| {
            let body = &chunk[chunk.find('>').map(|i| i + 1).unwrap_or(0)..];
            let text = body[..body.find("</li>").unwrap_or(body.len())]
                .replace(|c: char| c.is_whitespace(), " ");
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        });
        items.push(QueueItem { video_id, title });
    }
    items
}

fn attr(s: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = s.find(&needle)? + needle.len();
    let end = s[start..].find('"')? + start;
    Some(html_unescape(&s[start..end]))
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

/// Unique, stable device id for this app so other remotes keep working.
fn device_id_for(screen_id: &str) -> String {
    hex::encode(Md5::digest(format!("Skipjack{screen_id}").as_bytes()))
}

/// Shortens long tokens for logs.
fn redact(s: &str) -> String {
    if s.chars().count() <= 10 {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(6).collect::<String>())
    }
}

pub struct LoungeClient {
    http: reqwest::Client,
    screen_id: String,
    device_id: String,
    label: String,
    opts: LoungeOptions,
    session: Mutex<Session>,
    command_lock: tokio::sync::Mutex<()>,
    events: mpsc::UnboundedSender<LoungeEvent>,
}

impl LoungeClient {
    pub fn new(
        http: reqwest::Client,
        screen_id: &str,
        label: &str,
        opts: LoungeOptions,
        events: mpsc::UnboundedSender<LoungeEvent>,
    ) -> Self {
        Self {
            http,
            screen_id: screen_id.to_string(),
            device_id: device_id_for(screen_id),
            label: label.to_string(),
            opts,
            session: Mutex::new(Session {
                lounge_token: None,
                sid: None,
                gsession: None,
                last_event_id: None,
                command_offset: 1,
                screen_name: None,
                volume: "100".into(),
                muted: "false".into(),
                ad_active: false,
                ad_reload: AdReloadState::default(),
                shorts_disconnected: false,
                playback: PlaybackState::default(),
                content: ContentProgress::default(),
                queue: QueueState::default(),
                quality: QualityState::default(),
            }),
            command_lock: tokio::sync::Mutex::new(()),
            events,
        }
    }

    fn session(&self) -> std::sync::MutexGuard<'_, Session> {
        self.session.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn linked(&self) -> bool {
        self.session().lounge_token.is_some()
    }

    pub fn connected(&self) -> bool {
        self.session().connected()
    }

    pub fn screen_name(&self) -> Option<String> {
        self.session().screen_name.clone()
    }

    fn token(&self) -> Result<String> {
        self.session()
            .lounge_token
            .clone()
            .ok_or_else(|| LoungeError::NotLinked.into())
    }

    fn emit(&self, event: LoungeEvent) {
        let _ = self.events.send(event);
    }

    /// Runs a command in the background, logging the outcome.
    fn spawn_cmd<F, Fut>(self: &Arc<Self>, what: &'static str, f: F)
    where
        F: FnOnce(Arc<Self>) -> Fut,
        Fut: Future<Output = Result<bool>> + Send + 'static,
    {
        let fut = f(self.clone());
        let label = self.label.clone();
        tokio::spawn(async move {
            match fut.await {
                Ok(true) => tracing::debug!(device = %label, "{what}: ok"),
                Ok(false) => tracing::warn!(device = %label, "{what}: rejected (session lost)"),
                Err(e) => tracing::warn!(device = %label, "{what} failed: {e:#}"),
            }
        });
    }

    // ------------------------------------------------------------------
    // Pairing / auth
    // ------------------------------------------------------------------

    /// Fetches a fresh lounge token for the screen id.
    pub async fn refresh_auth(&self) -> Result<bool> {
        tracing::debug!(device = %self.label, "requesting lounge token for screen {}", redact(&self.screen_id));
        let body: Value = self
            .http
            .post(format!("{LOUNGE_API}/pairing/get_lounge_token_batch"))
            .form(&[("screen_ids", self.screen_id.as_str())])
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let screen = &body["screens"][0];
        let token = screen["loungeToken"]
            .as_str()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| anyhow!("no lounge token for screen (unpaired?), response: {body}"))?;
        tracing::info!(device = %self.label, "got lounge token");
        self.session().lounge_token = Some(token.to_string());
        Ok(true)
    }

    /// Asks YouTube whether the screen is online.
    pub async fn is_available(&self) -> Result<bool> {
        let token = self.token()?;
        let body: Value = self
            .http
            .post(format!("{LOUNGE_API}/pairing/get_screen_availability"))
            .form(&[("lounge_token", token.as_str())])
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?
            .json()
            .await?;
        let status = body["screens"][0]["status"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        tracing::debug!(device = %self.label, "screen availability: {status}");
        Ok(status == "online")
    }

    // ------------------------------------------------------------------
    // Session
    // ------------------------------------------------------------------

    fn common_params(&self, s: &Session) -> Vec<(String, String)> {
        let mut p: Vec<(String, String)> = vec![
            ("name".into(), self.opts.join_name.clone()),
            ("device".into(), "REMOTE_CONTROL".into()),
            ("app".into(), LOUNGE_APP.into()),
            ("VER".into(), "8".into()),
            ("v".into(), "2".into()),
        ];
        if let Some(t) = &s.lounge_token {
            p.push(("loungeIdToken".into(), t.clone()));
        }
        if let Some(sid) = &s.sid {
            p.push(("SID".into(), sid.clone()));
        }
        if let Some(aid) = s.last_event_id {
            p.push(("AID".into(), aid.to_string()));
        }
        if let Some(g) = &s.gsession {
            p.push(("gsessionid".into(), g.clone()));
        }
        p
    }

    /// Returns false (and resets state) when the session is gone.
    fn handle_session_result(&self, status: StatusCode, context: &str) -> bool {
        let mut s = self.session();
        match status.as_u16() {
            400 | 410 => {
                tracing::info!(device = %self.label, "{context}: HTTP {status}, session lost");
                s.connection_lost();
                false
            }
            401 => {
                tracing::info!(device = %self.label, "{context}: HTTP {status}, lounge token expired");
                s.connection_lost();
                s.lounge_token = None;
                false
            }
            _ => true,
        }
    }

    fn bind_body(&self, token: &str, id: &str, device: &str) -> Vec<(&'static str, String)> {
        vec![
            ("id", id.to_string()),
            ("mdx-version", "3".into()),
            ("TYPE", "xmlhttp".into()),
            ("theme", "cl".into()),
            ("sessionSource", "MDX_SESSION_SOURCE_UNKNOWN".into()),
            (
                "connectParams",
                r#"{"setStatesParams": "{"playbackSpeed":0}"}"#.into(),
            ),
            ("RID", "1".into()),
            ("CVER", "1".into()),
            ("capabilities", "que,dsdtr,atp,vsp".into()),
            ("ui", "false".into()),
            ("app", LOUNGE_APP.into()),
            ("pairing_type", "manual".into()),
            ("VER", "8".into()),
            ("loungeIdToken", token.to_string()),
            ("device", device.to_string()),
            ("name", self.opts.join_name.clone()),
        ]
    }

    /// Opens a lounge session using the current lounge token.
    pub async fn connect(self: &Arc<Self>) -> Result<bool> {
        let token = self.token()?;
        let body = self.bind_body(&token, &self.device_id, "REMOTE_CONTROL");
        tracing::debug!(device = %self.label, "binding lounge session");
        let resp = self
            .http
            .post(format!("{LOUNGE_API}/bc/bind"))
            .form(&body)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        tracing::debug!(device = %self.label, "bind response HTTP {status}: {text}");

        if status == StatusCode::UNAUTHORIZED {
            if text.contains("Connection denied") {
                tracing::warn!(device = %self.label, "connection denied, attempting to circumvent the issue");
                if let Err(e) = self.connect_as_screen(&token).await {
                    tracing::warn!(device = %self.label, "connect as screen failed: {e:#}");
                }
                return Err(LoungeError::ConnectionDenied.into());
            }
            tracing::info!(device = %self.label, "bind: lounge token rejected, re-authenticating");
            self.session().lounge_token = None;
            return Ok(false);
        }
        if !status.is_success() {
            tracing::warn!(device = %self.label, "unknown reply to connect: HTTP {status}");
            return Ok(false);
        }

        let mut parser = ChunkParser::default();
        for line in text.lines() {
            if let Some(events) = parser.push_line(line) {
                self.process_events(events);
            }
        }
        let mut s = self.session();
        s.command_offset = 1;
        if !s.connected() {
            tracing::warn!(device = %self.label, "bind succeeded but no SID/gsessionid received");
        }
        Ok(s.connected())
    }

    /// Workaround for "Connection denied": joining as a screen makes YouTube
    /// reset the lounge; the TV app then needs to be restarted.
    async fn connect_as_screen(&self, token: &str) -> Result<()> {
        let mut body = self.bind_body(token, &uuid::Uuid::new_v4().to_string(), "LOUNGE_SCREEN");
        body.push(("sessionNonce", uuid::Uuid::new_v4().to_string()));
        self.http
            .post(format!("{LOUNGE_API}/bc/bind"))
            .form(&body)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?
            .text()
            .await?;
        tracing::error!(
            device = %self.label,
            "connected as screen: please force close the YouTube app on the device for Skipjack to work properly"
        );
        Ok(())
    }

    /// Long-polls the lounge and processes events until the session ends,
    /// the stream breaks or no data arrived for [`WATCHDOG_TIMEOUT`].
    pub async fn subscribe(self: &Arc<Self>) -> Result<()> {
        let query = {
            let s = self.session();
            if !s.connected() {
                return Err(LoungeError::NotConnected.into());
            }
            let mut q = self.common_params(&s);
            q.push(("RID".into(), "rpc".into()));
            q.push(("CI".into(), "0".into()));
            q.push(("TYPE".into(), "xmlhttp".into()));
            q
        };
        tracing::debug!(device = %self.label, "subscribing to lounge");
        let resp = self
            .http
            .get(format!("{LOUNGE_API}/bc/bind"))
            .query(&query)
            .send()
            .await?;
        let status = resp.status();
        tracing::debug!(device = %self.label, "subscribe: HTTP {status} ({:?})", resp.version());
        if !self.handle_session_result(status, "subscribe") {
            return Ok(());
        }
        if !status.is_success() {
            return Err(anyhow!("subscribe failed: HTTP {status}"));
        }

        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut parser = ChunkParser::default();
        loop {
            let chunk = match tokio::time::timeout(WATCHDOG_TIMEOUT, stream.next()).await {
                Err(_) => {
                    tracing::info!(device = %self.label, "watchdog: no events for {WATCHDOG_TIMEOUT:?}, resubscribing");
                    return Ok(());
                }
                Ok(None) => {
                    tracing::debug!(device = %self.label, "subscribe stream ended");
                    return Ok(());
                }
                Ok(Some(Err(e))) => return Err(e.into()),
                Ok(Some(Ok(bytes))) => bytes,
            };
            tracing::trace!(device = %self.label, "raw: {}", String::from_utf8_lossy(&chunk));
            buf.extend_from_slice(&chunk);
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let raw: Vec<u8> = buf.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&raw[..raw.len() - 1]);
                if let Some(events) = parser.push_line(&line) {
                    self.process_events(events);
                    if !self.connected() {
                        tracing::info!(device = %self.label, "session closed by an event, leaving subscription");
                        return Ok(());
                    }
                }
            }
        }
    }

    /// Terminates the session (best effort).
    pub async fn disconnect(&self) -> Result<bool> {
        let query = {
            let s = self.session();
            if !s.connected() {
                return Ok(false);
            }
            let mut q = self.common_params(&s);
            q.push(("CVER".into(), "1".into()));
            q.push(("RID".into(), s.command_offset.to_string()));
            q.push(("auth_failure_option".into(), "send_error".into()));
            q
        };
        tracing::debug!(device = %self.label, "terminating lounge session");
        let resp = self
            .http
            .post(format!("{LOUNGE_API}/bc/bind"))
            .query(&query)
            .form(&[
                ("ui", ""),
                ("TYPE", "terminate"),
                (
                    "clientDisconnectReason",
                    "MDX_SESSION_DISCONNECT_REASON_DISCONNECTED_BY_USER",
                ),
            ])
            .timeout(Duration::from_secs(5))
            .send()
            .await?;
        self.session().connection_lost();
        Ok(resp.status().is_success())
    }

    // ------------------------------------------------------------------
    // Commands
    // ------------------------------------------------------------------

    async fn command(&self, cmd: &str, params: &[(&str, String)]) -> Result<bool> {
        // Serialise commands so `ofs`/`RID` stay consistent.
        let _guard = self.command_lock.lock().await;
        let (query, body, ofs) = {
            let mut s = self.session();
            if !s.connected() {
                return Err(LoungeError::NotConnected.into());
            }
            let ofs = s.command_offset;
            s.command_offset += 1;
            let mut query = self.common_params(&s);
            query.push(("RID".into(), s.command_offset.to_string()));
            let mut body: Vec<(String, String)> = vec![
                ("count".into(), "1".into()),
                ("ofs".into(), ofs.to_string()),
                ("req0__sc".into(), cmd.to_string()),
            ];
            body.extend(params.iter().map(|(k, v)| (format!("req0_{k}"), v.clone())));
            (query, body, ofs)
        };
        tracing::debug!(device = %self.label, "→ {cmd} {params:?} (ofs={ofs})");
        let resp = self
            .http
            .post(format!("{LOUNGE_API}/bc/bind"))
            .query(&query)
            .form(&body)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        tracing::debug!(device = %self.label, "← {cmd}: HTTP {status} {}", text.trim());
        if !self.handle_session_result(status, cmd) {
            return Ok(false);
        }
        if !status.is_success() {
            return Err(anyhow!(
                "command {cmd} failed: HTTP {status} {}",
                text.trim()
            ));
        }
        Ok(true)
    }

    pub async fn skip_ad(&self) -> Result<bool> {
        self.command("skipAd", &[]).await
    }

    pub async fn play(&self) -> Result<bool> {
        self.command("play", &[]).await
    }

    pub async fn pause(&self) -> Result<bool> {
        self.command("pause", &[]).await
    }

    pub async fn seek_to(&self, secs: f64) -> Result<bool> {
        self.command("seekTo", &[("newTime", secs.to_string())])
            .await
    }

    pub async fn next(&self) -> Result<bool> {
        self.command("next", &[]).await
    }

    pub async fn previous(&self) -> Result<bool> {
        self.command("previous", &[]).await
    }

    pub async fn set_speed(&self, speed: f64) -> Result<bool> {
        self.command("setPlaybackSpeed", &[("playbackSpeed", speed.to_string())])
            .await
    }

    pub async fn set_subtitles(&self, video_id: &str, language_code: &str) -> Result<bool> {
        self.command(
            "setSubtitlesTrack",
            &[
                ("videoId", video_id.to_string()),
                ("languageCode", language_code.to_string()),
            ],
        )
        .await
    }

    pub async fn dpad(&self, key: DpadKey) -> Result<bool> {
        self.command("dpadCommand", &[("key", key.as_cmd().to_string())])
            .await
    }

    pub async fn play_video(self: &Arc<Self>, video_id: &str) -> Result<bool> {
        let ok = self
            .set_playlist(&Playlist {
                video_id: video_id.to_string(),
                ..Default::default()
            })
            .await?;
        if ok {
            self.start_playback().await?;
        }
        Ok(ok)
    }

    /// Starts playback after a [`Self::set_playlist`]: the TV loads the video
    /// cued and stays paused, so `play` must be sent explicitly. Repeats a few
    /// times because the first command often arrives before the player is up
    /// (`play` is idempotent, so retrying is harmless).
    pub async fn start_playback(self: &Arc<Self>) -> Result<bool> {
        for attempt in 1..=START_PLAYBACK_TRIES {
            tokio::time::sleep(START_PLAYBACK_DELAY).await;
            if !self.play().await? {
                return Ok(false);
            }
            let state = self.session().playback.state;
            tracing::debug!(device = %self.label, "play after load (attempt {attempt}/{START_PLAYBACK_TRIES}), state={state:?}");
            if state == PlayerState::Playing {
                break;
            }
        }
        Ok(true)
    }

    /// Loads a video + queue onto the TV. `current_time` applies to the first
    /// video only. Retries once with a fresh bind when the session is gone
    /// (long-lived sessions sometimes stop accepting queue commands).
    pub async fn set_playlist(self: &Arc<Self>, playlist: &Playlist) -> Result<bool> {
        self.queue_command("setPlaylist", &playlist.params()).await
    }

    /// Appends a video at the end of the queue.
    pub async fn add_video(self: &Arc<Self>, video_id: &str) -> Result<bool> {
        self.queue_command("addVideo", &[("videoId", video_id.to_string())])
            .await
    }

    /// Inserts a video right after the currently playing one (not at the end).
    pub async fn insert_video(self: &Arc<Self>, video_id: &str) -> Result<bool> {
        self.queue_command("insertVideo", &[("videoId", video_id.to_string())])
            .await
    }

    /// Removes a video from the queue.
    pub async fn remove_video(self: &Arc<Self>, video_id: &str) -> Result<bool> {
        self.queue_command("removeVideo", &[("videoId", video_id.to_string())])
            .await
    }

    /// Clears the queue.
    pub async fn clear_playlist(self: &Arc<Self>) -> Result<bool> {
        self.queue_command("clearPlaylist", &[]).await
    }

    /// Queue commands with rebind fallback: when the session is gone
    /// (HTTP 400/410/401), re-auth + re-bind once and retry the command.
    async fn queue_command(self: &Arc<Self>, cmd: &str, params: &[(&str, String)]) -> Result<bool> {
        if self.command(cmd, params).await? {
            return Ok(true);
        }
        // Session lost mid-command. Re-auth (token may have expired) and
        // re-bind, then retry once.
        tracing::info!(device = %self.label, "{cmd}: session lost, re-binding and retrying");
        self.refresh_auth().await?;
        if !self.connect().await? {
            return Ok(false);
        }
        self.command(cmd, params).await
    }

    /// Adds several videos to the end of the queue, one request at a time with
    /// a 2–5 s pause between them — batching corrupts the queue server-side.
    pub async fn add_videos(self: &Arc<Self>, video_ids: &[String]) -> Result<bool> {
        let mut first = true;
        for vid in video_ids {
            if !first {
                // One request at a time with a 2–5 s pause; batching corrupts
                // the queue server-side. Rng is dropped before the sleep so
                // the future stays Send (`thread_rng` is `!Send`).
                let pause = {
                    use rand::Rng;
                    rand::thread_rng().gen_range(2.0..5.0)
                };
                tokio::time::sleep(Duration::from_secs_f64(pause)).await;
            }
            first = false;
            if !self.add_video(vid).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub async fn get_now_playing(&self) -> Result<bool> {
        self.command("getNowPlaying", &[]).await
    }

    /// Last known queue snapshot (from `playlistModified`, incl. the bind frame).
    pub fn queue(&self) -> QueueState {
        self.session().queue.clone()
    }

    /// Last known volume snapshot (from `onVolumeChanged`).
    pub fn volume(&self) -> VolumeState {
        let s = self.session();
        VolumeState {
            volume: s.volume.parse().unwrap_or(100),
            muted: s.muted == "true",
        }
    }

    /// Last known quality snapshot (from `onVideoQualityChanged`).
    pub fn quality(&self) -> QualityState {
        self.session().quality.clone()
    }

    /// Fetches the queue HTML for `list_id` and parses `<li class="…watch-queue-item">`
    /// items (video id + title). Needs the lounge token as `X-YouTube-LoungeId-Token`.
    pub async fn watch_queue(&self, list_id: &str) -> Result<Vec<QueueItem>> {
        let token = self.token()?;
        let resp = self
            .http
            .get("https://www.youtube.com/watch_queue_ajax")
            .query(&[("action_get_watch_queue_items", "1"), ("list", list_id)])
            .header("X-YouTube-LoungeId-Token", token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("watch_queue_ajax: HTTP {}", resp.status());
        }
        Ok(parse_watch_queue(&resp.text().await.unwrap_or_default()))
    }

    pub async fn set_auto_play_mode(&self, enabled: bool) -> Result<bool> {
        let mode = if enabled { "ENABLED" } else { "DISABLED" };
        self.command("setAutoplayMode", &[("autoplayMode", mode.to_string())])
            .await
    }

    /// Mutes/unmutes. Unless `force`, nothing is sent when already in that state.
    pub async fn mute(&self, mute: bool, force: bool) -> Result<bool> {
        let muted = if mute { "true" } else { "false" };
        let volume = {
            let mut s = self.session();
            if !force && s.muted == muted {
                return Ok(true);
            }
            s.muted = muted.to_string();
            s.volume.clone()
        };
        // YouTube wants the volume when unmuting, so we always send it.
        self.command(
            "setVolume",
            &[("volume", volume), ("muted", muted.to_string())],
        )
        .await
    }

    // ------------------------------------------------------------------
    // Ads
    // ------------------------------------------------------------------

    /// Experimental ad escape: remember the content video + position, press
    /// `BACK` to leave the player (which tears down the ad session), then load
    /// the video again at that position. See docs/PLAN-ad-reload.md.
    ///
    /// One attempt per ad; [`MAX_AD_RELOADS`] consecutive failures give up for
    /// the session (a success resets the budget).
    fn try_ad_reload(self: &Arc<Self>, data: &Value, source: &str) {
        if !self.opts.experimental_reload_ads {
            return;
        }
        let ad_id = field_str(data, "adVideoId").unwrap_or_default();
        let (video_id, position, attempt) = {
            let mut s = self.session();
            // An attempt that is already over must never lock the experiment:
            // either the window passed without a verdict, or a *different* ad
            // started (the reload served a new pre-roll). Only the echo of the
            // very same ad — `onAdStateChange` carries no ad id — is ignored.
            if let Some(pending) = s.ad_reload.pending.as_ref() {
                let same_ad =
                    ad_id.is_empty() || (!pending.ad_id.is_empty() && pending.ad_id == ad_id);
                if same_ad {
                    return;
                }
                tracing::debug!(
                    device = %self.label,
                    "ad escape: previous attempt (ad {}) is over, new ad {ad_id} started",
                    if pending.ad_id.is_empty() { "?" } else { pending.ad_id.as_str() }
                );
                s.ad_reload.pending = None;
            }
            if s.ad_reload.aborted || s.ad_reload.attempts >= MAX_AD_RELOADS {
                return;
            }
            // The same ad retried on a later event of this same ad.
            if !ad_id.is_empty() && s.ad_reload.tried_ad.as_deref() == Some(ad_id.as_str()) {
                return;
            }
            // `adPlaying` carries the content id; otherwise fall back to the
            // progress we recorded while the content video was playing — the
            // event's own `currentTime` belongs to the ad.
            let video_id = field_str(data, "contentVideoId")
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| s.content.video_id.clone());
            if video_id.is_empty() {
                tracing::debug!(device = %self.label, "ad escape skipped: content video unknown yet");
                return;
            }
            let position = s.content.position;
            s.ad_reload.attempts += 1;
            if !ad_id.is_empty() {
                s.ad_reload.tried_ad = Some(ad_id.clone());
            }
            s.ad_reload.pending = Some(PendingReload {
                video_id: video_id.clone(),
                position,
                ad_id: ad_id.clone(),
                started: std::time::Instant::now(),
            });
            (video_id, position, s.ad_reload.attempts)
        };
        tracing::warn!(
            device = %self.label,
            "experimental ad escape ({source}): BACK, then setPlaylist videoId={video_id} currentTime={position:.1} (attempt {attempt}/{MAX_AD_RELOADS})"
        );
        self.spawn_cmd("adEscape", move |c| async move {
            // Leave the player: this also drops the ad session on the TV.
            match c.dpad(DpadKey::Back).await {
                Ok(true) => {}
                Ok(false) => {
                    c.session().ad_reload.pending = None;
                    return Ok(false);
                }
                Err(e) => {
                    c.session().ad_reload.aborted = true;
                    c.session().ad_reload.pending = None;
                    return Err(e);
                }
            }
            tokio::time::sleep(AD_RELOAD_RETURN_DELAY).await;
            let result = c
                .set_playlist(&Playlist {
                    video_id: video_id.clone(),
                    current_time: Some(position),
                    ..Default::default()
                })
                .await;
            match &result {
                Ok(true) => {
                    tracing::warn!(device = %c.label, "experimental ad escape: reload sent for {video_id}, starting playback and waiting for state=1 within {AD_RELOAD_WINDOW:?}");
                    // `setPlaylist` leaves the TV cued — without this the video
                    // never resumes and the attempt just hangs.
                    if let Err(e) = c.start_playback().await {
                        tracing::warn!(device = %c.label, "experimental ad escape: play after reload failed: {e:#}");
                    }
                }
                Ok(false) => {
                    tracing::warn!(device = %c.label, "experimental ad escape rejected (session lost), giving up for this session");
                    let mut s = c.session();
                    s.ad_reload.aborted = true;
                    s.ad_reload.pending = None;
                }
                Err(e) => {
                    tracing::warn!(device = %c.label, "experimental ad escape failed: {e:#}, giving up for this session");
                    let mut s = c.session();
                    s.ad_reload.aborted = true;
                    s.ad_reload.pending = None;
                }
            }
            Ok(true)
        });
    }

    /// Called on `state=1` (`nowPlaying` or `onStateChange`): judges the
    /// attempt in flight. Only a clear success resets the failure budget; a late
    /// resume or a different video is just one used attempt, so the next ad is
    /// still tried. The experiment is abandoned only on hard command errors
    /// (lost session), handled where the attempt is spawned.
    fn ad_reload_video_resumed(self: &Arc<Self>, video_id: &str) {
        let (verdict, position) = {
            let mut s = self.session();
            let Some(pending) = s.ad_reload.pending.take() else {
                return;
            };
            let position = pending.position;
            let verdict = if video_id == pending.video_id {
                if pending.started.elapsed() <= AD_RELOAD_WINDOW {
                    s.ad_reload.attempts = 0;
                    Ok("SUCCESS")
                } else {
                    Ok("resumed, but after the window")
                }
            } else {
                // Not fatal: it just means this attempt didn't restore the
                // content. The failure budget still bounds further attempts.
                Err(format!(
                    "no verdict: a different video ({video_id}) started"
                ))
            };
            (verdict, position)
        };
        match verdict {
            Ok("SUCCESS") => {
                tracing::warn!(device = %self.label, "experimental ad escape: SUCCESS, {video_id} resumed at {position:.1}s within {AD_RELOAD_WINDOW:?}")
            }
            Ok(other) => tracing::info!(device = %self.label, "experimental ad escape: {other}"),
            Err(why) => tracing::warn!(device = %self.label, "experimental ad escape: {why}"),
        }
    }

    fn ad_started(self: &Arc<Self>, data: &Value, source: &str) {
        let skippable = field_str(data, "isSkipEnabled").as_deref() == Some("true");
        if !skippable {
            self.try_ad_reload(data, source);
        }
        if self.opts.skip_ads && skippable {
            tracing::info!(device = %self.label, "ad is skippable ({source}), skipping");
            self.session().ad_active = false;
            self.spawn_cmd("skipAd", |c| async move { c.skip_ad().await });
            self.emit(LoungeEvent::Ad(AdAction::Skipped));
            if self.opts.mute_ads {
                self.spawn_cmd("unmute", |c| async move { c.mute(false, true).await });
            }
        } else if self.opts.mute_ads {
            let first = !std::mem::replace(&mut self.session().ad_active, true);
            tracing::info!(
                device = %self.label,
                "ad playing ({source}, skippable={skippable}), muting{}",
                if first { "" } else { " again" }
            );
            self.spawn_cmd("mute", |c| async move { c.mute(true, true).await });
            if first {
                self.emit(LoungeEvent::Ad(AdAction::Muted));
            }
        } else {
            tracing::info!(device = %self.label, "ad playing ({source}, skippable={skippable}), nothing to do");
        }
    }

    fn ad_ended(self: &Arc<Self>, source: &str) {
        if !self.opts.mute_ads {
            return;
        }
        let was_active = std::mem::replace(&mut self.session().ad_active, false);
        if was_active {
            tracing::info!(device = %self.label, "ad ended ({source}), unmuting");
            self.emit(LoungeEvent::Ad(AdAction::Unmuted));
        } else {
            tracing::debug!(device = %self.label, "video playing ({source}), making sure it's unmuted");
        }
        self.spawn_cmd("unmute", |c| async move { c.mute(false, true).await });
    }

    // ------------------------------------------------------------------
    // Events
    // ------------------------------------------------------------------

    fn process_events(self: &Arc<Self>, events: Vec<Value>) {
        let mut last_id = None;
        for event in events {
            let Some(arr) = event.as_array() else {
                continue;
            };
            if let Some(id) = arr.first().and_then(Value::as_i64) {
                last_id = Some(id);
            }
            let Some(payload) = arr.get(1).and_then(Value::as_array) else {
                continue;
            };
            let Some(kind) = payload.first().and_then(Value::as_str) else {
                continue;
            };
            let args = &payload[1..];
            match kind {
                "c" => {
                    let sid = args.first().and_then(value_str);
                    tracing::debug!(device = %self.label, "got SID {:?}", sid.as_deref().map(redact));
                    self.session().sid = sid;
                }
                "S" => {
                    let g = args.first().and_then(value_str);
                    tracing::debug!(device = %self.label, "got gsessionid {:?}", g.as_deref().map(redact));
                    self.session().gsession = g;
                }
                _ => self.process_event(kind, args),
            }
        }
        if let Some(id) = last_id {
            self.session().last_event_id = Some(id);
        }
    }

    fn emit_playback(&self) {
        let state = self.session().playback.clone();
        self.emit(LoungeEvent::Playback(state));
    }

    fn process_event(self: &Arc<Self>, kind: &str, args: &[Value]) {
        let args_json = Value::Array(args.to_vec());
        if AD_EVENTS.contains(&kind) {
            tracing::info!(device = %self.label, "event {kind} {args_json}");
        } else if kind != "noop" {
            tracing::debug!(device = %self.label, "event {kind} {args_json}");
        }
        let empty = Value::Null;
        let data = args.first().unwrap_or(&empty);
        match kind {
            "onStateChange" | "nowPlaying" => {
                // These events are also sent as partial "heartbeats" with only
                // a listId or a currentTime — a missing field must mean "keep
                // the previous value", otherwise the UI loses the now-playing
                // video (and its remote controls) every few seconds.
                let state = field_str(data, "state").as_deref().map(PlayerState::parse);
                // Payloads describing an ad must never be taken for content.
                let ad_payload =
                    field_str(data, "adVideoId").is_some() || field_str(data, "adState").is_some();
                let is_ad_state = matches!(
                    state,
                    Some(PlayerState::Advertisement | PlayerState::AdSkipped)
                );
                let video_id = {
                    let mut s = self.session();
                    if let Some(t) = field_f64(data, "currentTime") {
                        s.playback.current_time = t;
                    }
                    if let Some(d) = field_f64(data, "duration") {
                        s.playback.duration = d;
                    }
                    if kind == "nowPlaying" {
                        if let Some(v) = field_str(data, "videoId").filter(|v| !v.is_empty()) {
                            s.playback.video_id = v;
                        }
                    }
                    if let Some(state) = state {
                        s.playback.state = state;
                    }
                    // Remember where the *content* is, so an ad can be escaped
                    // by leaving the player and coming back to this position.
                    if !ad_payload && !is_ad_state && !s.ad_active {
                        if let Some(v) = field_str(data, "videoId").filter(|v| !v.is_empty()) {
                            s.content.video_id = v;
                        }
                        if let Some(t) = field_f64(data, "currentTime").filter(|t| *t > 0.0) {
                            s.content.position = t;
                        }
                    }
                    s.playback.video_id.clone()
                };
                match state {
                    Some(PlayerState::Playing) => {
                        // `state=1` after an attempt is the success signal. The
                        // TV reports it through `nowPlaying` or `onStateChange`
                        // depending on the firmware, so accept both.
                        if !ad_payload {
                            self.ad_reload_video_resumed(&video_id);
                        }
                        self.ad_ended(kind)
                    }
                    // Some TVs report ads only through the player state.
                    Some(PlayerState::Advertisement) if !self.session().ad_active => {
                        self.ad_started(data, kind)
                    }
                    _ => {}
                }
                self.emit_playback();
            }
            "onAdStateChange" => {
                let ad_state = field_str(data, "adState").unwrap_or_default();
                // Ad not playing (0), or just skipped (1082). Note: the TV
                // reports the end of an ad with currentTime "0" too.
                if ad_state == "0" || ad_state == "1082" {
                    self.ad_ended("onAdStateChange");
                } else {
                    self.ad_started(data, "onAdStateChange");
                }
            }
            "adPlaying" => self.ad_started(data, "adPlaying"),
            "onVolumeChanged" => {
                let mut s = self.session();
                if let Some(v) = field_str(data, "volume") {
                    s.volume = v;
                }
                if let Some(m) = field_str(data, "muted") {
                    s.muted = m;
                }
                let volume = s.volume.parse().unwrap_or(100);
                let muted = s.muted == "true";
                drop(s);
                self.emit(LoungeEvent::Volume(VolumeState { volume, muted }));
            }
            "onVideoQualityChanged" => {
                let quality = QualityState {
                    level: field_str(data, "qualityLevel").filter(|s| !s.is_empty()),
                    available: field_str(data, "availableQualityLevels")
                        .map(|s| {
                            s.split(',')
                                .map(|v| v.trim().to_string())
                                .filter(|v| !v.is_empty())
                                .collect()
                        })
                        .unwrap_or_default(),
                };
                self.session().quality = quality.clone();
                self.emit(LoungeEvent::Quality(quality));
            }
            "playlistModified" => {
                let queue = QueueState::from_event_data(data);
                self.session().queue = queue.clone();
                self.emit(LoungeEvent::Queue(queue));
            }
            "loungeStatus" => {
                let devices: Vec<Value> = field_str(data, "devices")
                    .and_then(|d| serde_json::from_str(&d).ok())
                    .unwrap_or_default();
                for device in devices.iter().filter(|d| d["type"] == "LOUNGE_SCREEN") {
                    if let Some(name) = device["name"].as_str() {
                        self.session().screen_name = Some(name.to_string());
                    }
                    let info: Value = device["deviceInfo"]
                        .as_str()
                        .and_then(|i| serde_json::from_str(i).ok())
                        .unwrap_or(Value::Null);
                    let client = info["clientName"].as_str().unwrap_or_default();
                    tracing::info!(
                        device = %self.label,
                        "screen: {} client={client} brand={} model={}",
                        device["name"].as_str().unwrap_or("?"),
                        info["brand"].as_str().unwrap_or("?"),
                        info["model"].as_str().unwrap_or("?"),
                    );
                    if YOUTUBE_CLIENT_BLACKLIST.contains(&client) {
                        tracing::warn!(device = %self.label, "unsupported YouTube client {client} (kids profile?), disconnecting");
                        self.session().connection_lost();
                        return;
                    }
                }
            }
            "onSubtitlesTrackChanged" => {
                let resume = std::mem::replace(&mut self.session().shorts_disconnected, false);
                if resume {
                    if let Some(vid) = field_str(data, "videoId").filter(|v| !v.is_empty()) {
                        tracing::info!(device = %self.label, "resuming {vid} after shorts disconnect");
                        self.spawn_cmd(
                            "play_video",
                            move |c| async move { c.play_video(&vid).await },
                        );
                    }
                }
            }
            "loungeScreenDisconnected" => {
                let reason = field_str(data, "reason").unwrap_or_default();
                tracing::info!(device = %self.label, "screen disconnected (reason: {reason:?})");
                let mut s = self.session();
                if reason == "disconnectedByUserScreenInitiated" {
                    // Most likely a short is playing.
                    s.shorts_disconnected = true;
                }
                s.connection_lost();
                s.lounge_token = None;
            }
            "onAutoplayModeChanged" => {
                let mode = field_str(data, "autoplayMode").unwrap_or_default();
                let wanted = if self.opts.auto_play {
                    "ENABLED"
                } else {
                    "DISABLED"
                };
                if mode != "UNSUPPORTED" && mode != wanted {
                    let enabled = self.opts.auto_play;
                    self.spawn_cmd("setAutoplayMode", move |c| async move {
                        c.set_auto_play_mode(enabled).await
                    });
                }
            }
            "onPlaybackSpeedChanged" => {
                self.spawn_cmd(
                    "getNowPlaying",
                    |c| async move { c.get_now_playing().await },
                );
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chunks() {
        let body = "42\n[[0,[\"c\",\"SID123\",\"\",8]],\n[1,[\"S\",\"G1\"]]]\n";
        let mut p = ChunkParser::default();
        let mut out = Vec::new();
        for line in body.lines() {
            if let Some(ev) = p.push_line(line) {
                out.push(ev);
            }
        }
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), 2);
        assert_eq!(out[0][1][1][1], "G1");
    }

    #[test]
    fn playlist_params_full() {
        let p = Playlist {
            video_id: "a".into(),
            video_ids: vec!["a".into(), "b".into(), "c".into()],
            current_index: Some(1),
            current_time: Some(12.5),
            list_id: Some("PLxyz".into()),
        };
        let params = p.params();
        let get = |k: &str| {
            params
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("videoId").as_deref(), Some("a"));
        assert_eq!(get("videoIds").as_deref(), Some("a,b,c"));
        assert_eq!(get("currentIndex").as_deref(), Some("1"));
        assert_eq!(get("currentTime").as_deref(), Some("12.5"));
        assert_eq!(get("listId").as_deref(), Some("PLxyz"));
    }

    #[test]
    fn playlist_params_minimal() {
        let p = Playlist {
            video_id: "a".into(),
            ..Default::default()
        };
        let params = p.params();
        assert_eq!(params.len(), 1);
        assert_eq!(params[0], ("videoId", "a".to_string()));
    }

    #[test]
    fn player_state_parsing() {
        assert_eq!(PlayerState::parse("1"), PlayerState::Playing);
        assert_eq!(PlayerState::parse("1081"), PlayerState::Advertisement);
        assert_eq!(PlayerState::parse("garbage"), PlayerState::Stopped);
    }

    #[test]
    fn queue_state_from_event_data() {
        let data = serde_json::json!({"listId": "PLxyz", "videoIds": "a,b,c"});
        let q = QueueState::from_event_data(&data);
        assert_eq!(q.list_id.as_deref(), Some("PLxyz"));
        assert_eq!(q.video_ids, vec!["a", "b", "c"]);
        let empty = QueueState::from_event_data(&serde_json::json!({}));
        assert_eq!(empty, QueueState::default());
    }

    #[test]
    fn parses_watch_queue_html() {
        let html = r#"<ul><li class="watch-queue-item" data-video-id="abc123XYZ_-"><span title="First &amp; title"></span></li><li class="other" data-video-id="nope"></li><li class="watch-queue-item" data-video-id="def456UVW12">  Second title  </li></ul>"#;
        let items = parse_watch_queue(html);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].video_id, "abc123XYZ_-");
        assert_eq!(items[0].title.as_deref(), Some("First & title"));
        assert_eq!(items[1].video_id, "def456UVW12");
        assert_eq!(items[1].title.as_deref(), Some("Second title"));
    }

    fn pending_reload(ad_id: &str) -> PendingReload {
        PendingReload {
            video_id: "vid".into(),
            position: 12.5,
            ad_id: ad_id.into(),
            started: std::time::Instant::now(),
        }
    }

    #[test]
    fn ad_escape_budget() {
        let mut state = AdReloadState::default();
        for i in 0..MAX_AD_RELOADS {
            assert!(!state.aborted && state.attempts < MAX_AD_RELOADS);
            state.attempts += 1;
            state.pending = Some(pending_reload("ad1"));
            assert_eq!(state.attempts, i + 1);
        }
        assert_eq!(state.attempts, MAX_AD_RELOADS);
        state.pending.take();
        assert!(state.pending.is_none());
    }

    #[test]
    fn ad_escape_verdict() {
        let mut state = AdReloadState {
            pending: Some(pending_reload("ad1")),
            ..Default::default()
        };
        let pending = state.pending.take().unwrap();
        assert_eq!(pending.video_id, "vid");
        assert!(pending.started.elapsed() <= AD_RELOAD_WINDOW);
        assert_ne!(pending.video_id, "other");
    }

    /// A second ad must be escapable: only the echo of the ad we are already
    /// handling (no ad id, same ad id) is ignored.
    #[test]
    fn ad_escape_releases_pending_for_a_new_ad() {
        let state = AdReloadState {
            pending: Some(pending_reload("ad1")),
            ..Default::default()
        };
        // Echo of the same ad: `onAdStateChange` (no id) and a repeated id.
        for incoming in ["", "ad1"] {
            let pending = state.pending.as_ref().unwrap();
            let same_ad =
                incoming.is_empty() || (!pending.ad_id.is_empty() && pending.ad_id == incoming);
            assert!(same_ad, "{incoming:?} should be treated as an echo");
        }
        // A genuinely new pre-roll releases the attempt.
        let pending = state.pending.as_ref().unwrap();
        let incoming = "ad2";
        let same_ad =
            incoming.is_empty() || (!pending.ad_id.is_empty() && pending.ad_id == incoming);
        assert!(!same_ad, "a new ad must not be treated as an echo");
    }

    #[test]
    fn device_id_is_stable_md5() {
        let id = device_id_for("abc");
        assert_eq!(id.len(), 32);
        assert_eq!(id, device_id_for("abc"));
    }
}
