// Prevents an additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use skipjack_core::config::{config_path, default_data_dir, load_from_data_dir};
use skipjack_core::{
    build_http_client, discover, event_bus, logging, Config, DpadKey, EventBus, LoungeClient,
    PairedScreen, Playlist, QualityState, QueueItem, QueueState, RemoteHub, Service, ServiceEvent,
    Stats, VolumeState,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Emitter, Manager, RunEvent, State, WindowEvent,
};
use tokio::sync::Mutex;
use tokio::task::AbortHandle;

struct AppState {
    data_dir: PathBuf,
    log_path: Option<PathBuf>,
    /// True when launched with `--tray`/`--minimized`: the window starts
    /// hidden and nothing connects until the user presses Start.
    tray_mode: bool,
    bus: EventBus,
    service: Mutex<Option<Service>>,
    discovery: Mutex<Option<AbortHandle>>,
}

type CmdResult<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[derive(Serialize)]
struct Meta {
    version: &'static str,
    data_dir: String,
    config_path: String,
    log_path: Option<String>,
    tray_mode: bool,
}

#[tauri::command]
fn get_meta(state: State<'_, AppState>) -> Meta {
    Meta {
        version: env!("CARGO_PKG_VERSION"),
        data_dir: state.data_dir.display().to_string(),
        config_path: config_path(&state.data_dir).display().to_string(),
        log_path: state.log_path.as_ref().map(|p| p.display().to_string()),
        tray_mode: state.tray_mode,
    }
}

#[tauri::command]
fn get_stats(state: State<'_, AppState>) -> Stats {
    Stats::load(&state.data_dir)
}

#[tauri::command]
fn reset_stats(app: tauri::AppHandle, state: State<'_, AppState>) -> Stats {
    let stats = Stats::default();
    stats.save(&state.data_dir);
    let _ = app.emit("stats", stats);
    stats
}

#[tauri::command]
fn get_config(state: State<'_, AppState>) -> CmdResult<Config> {
    load_from_data_dir(&state.data_dir).map_err(err)
}

#[tauri::command]
fn save_config(state: State<'_, AppState>, config: Config) -> CmdResult<()> {
    config.save(&config_path(&state.data_dir)).map_err(err)
}

/// Returns validation warnings, or an error if the service could not start.
#[tauri::command]
fn validate_config(config: Config) -> CmdResult<Vec<String>> {
    config.validate().map_err(err)
}

#[tauri::command]
async fn pair_device(code: String, use_proxy: bool) -> CmdResult<PairedScreen> {
    let client = build_http_client(use_proxy);
    skipjack_core::pair_with_code(&client, &code)
        .await
        .map_err(err)?
        .ok_or_else(|| {
            "Pairing failed. Check the code or restart the YouTube app on the TV.".to_string()
        })
}

/// Starts discovery; devices arrive as `discovery-device` events and the
/// end is signalled with `discovery-done`. A watchdog guarantees `done`
/// is always emitted (worst case after the timeout), so the UI can never
/// get stuck on "Scanning…".
#[tauri::command]
async fn start_discovery(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    active: bool,
    use_proxy: bool,
) -> CmdResult<()> {
    const DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
    let mut slot = state.discovery.lock().await;
    if let Some(prev) = slot.take() {
        prev.abort();
    }
    let mut rx = discover(build_http_client(use_proxy), active);
    let task = tokio::spawn(async move {
        let done = async {
            while let Some(device) = rx.recv().await {
                let _ = app.emit("discovery-device", device);
            }
        };
        // If the scan hangs (e.g. an unresponsive device holds a probe),
        // stop waiting but still tell the UI it is over.
        let _ = tokio::time::timeout(DISCOVERY_TIMEOUT, done).await;
        let _ = app.emit("discovery-done", ());
    });
    *slot = Some(task.abort_handle());
    Ok(())
}

#[tauri::command]
async fn start_service(state: State<'_, AppState>) -> CmdResult<()> {
    let config = load_from_data_dir(&state.data_dir).map_err(err)?;
    let mut slot = state.service.lock().await;
    if let Some(old) = slot.take() {
        old.stop().await;
    }
    *slot = Some(Service::start(config, state.bus.clone()).map_err(err)?);
    Ok(())
}

/// Starts the service unless it is already running or the config is not
/// usable yet. Returns whether the service is running afterwards.
#[tauri::command]
async fn autostart_service(state: State<'_, AppState>) -> CmdResult<bool> {
    let mut slot = state.service.lock().await;
    if slot.as_ref().is_some_and(Service::is_running) {
        return Ok(true);
    }
    let config = load_from_data_dir(&state.data_dir).map_err(err)?;
    if config.validate().is_err() {
        return Ok(false);
    }
    *slot = Some(Service::start(config, state.bus.clone()).map_err(err)?);
    Ok(true)
}

#[tauri::command]
async fn stop_service(state: State<'_, AppState>) -> CmdResult<()> {
    if let Some(service) = state.service.lock().await.take() {
        service.stop().await;
    }
    Ok(())
}

#[tauri::command]
async fn service_running(state: State<'_, AppState>) -> CmdResult<bool> {
    Ok(state
        .service
        .lock()
        .await
        .as_ref()
        .is_some_and(Service::is_running))
}

/// Snapshot of the running service's remote clients. Taking it does not keep
/// the service lock, so a slow lounge request can never block the UI.
async fn hub(state: &State<'_, AppState>) -> CmdResult<RemoteHub> {
    let slot = state.service.lock().await;
    slot.as_ref()
        .filter(|s| s.is_running())
        .map(Service::hub)
        .ok_or_else(|| "Service is not running".to_string())
}

/// Runs a lounge command against one device.
async fn remote<F, Fut>(
    state: &State<'_, AppState>,
    screen_id: &str,
    what: &'static str,
    f: F,
) -> CmdResult<()>
where
    F: FnOnce(Arc<LoungeClient>) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<bool>>,
{
    let client = hub(state).await?.client(screen_id).await?;
    match f(client).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!(
            "{what} rejected (session lost, is YouTube open on the TV?)"
        )),
        Err(e) => Err(format!("{what} failed: {e:#}")),
    }
}

#[tauri::command]
async fn remote_play(state: State<'_, AppState>, screen_id: String) -> CmdResult<()> {
    remote(&state, &screen_id, "play", async |r| r.play().await).await
}

#[tauri::command]
async fn remote_pause(state: State<'_, AppState>, screen_id: String) -> CmdResult<()> {
    remote(&state, &screen_id, "pause", async |r| r.pause().await).await
}

#[tauri::command]
async fn remote_seek(state: State<'_, AppState>, screen_id: String, secs: f64) -> CmdResult<()> {
    remote(&state, &screen_id, "seek", async move |r| {
        r.seek_to(secs).await
    })
    .await
}

#[tauri::command]
async fn remote_next(state: State<'_, AppState>, screen_id: String) -> CmdResult<()> {
    remote(&state, &screen_id, "next", async |r| r.next().await).await
}

#[tauri::command]
async fn remote_previous(state: State<'_, AppState>, screen_id: String) -> CmdResult<()> {
    remote(&state, &screen_id, "previous", async |r| r.previous().await).await
}

#[tauri::command]
async fn remote_speed(state: State<'_, AppState>, screen_id: String, speed: f64) -> CmdResult<()> {
    remote(&state, &screen_id, "speed", async move |r| {
        r.set_speed(speed).await
    })
    .await
}

#[tauri::command]
async fn remote_subtitles(
    state: State<'_, AppState>,
    screen_id: String,
    video_id: String,
    language_code: String,
) -> CmdResult<()> {
    remote(&state, &screen_id, "subtitles", async move |r| {
        r.set_subtitles(&video_id, &language_code).await
    })
    .await
}

#[tauri::command]
async fn remote_dpad(state: State<'_, AppState>, screen_id: String, key: String) -> CmdResult<()> {
    let key = DpadKey::parse(&key).ok_or_else(|| format!("bad dpad key {key:?}"))?;
    remote(&state, &screen_id, "dpad", async move |r| r.dpad(key).await).await
}

#[tauri::command]
async fn remote_play_video(
    state: State<'_, AppState>,
    screen_id: String,
    video_id: String,
) -> CmdResult<()> {
    remote(&state, &screen_id, "play video", async move |r| {
        r.play_video(&video_id).await
    })
    .await
}

#[tauri::command]
async fn remote_set_playlist(
    state: State<'_, AppState>,
    screen_id: String,
    video_id: String,
    queue: Option<String>,
    index: Option<usize>,
    position: Option<f64>,
    list_id: Option<String>,
) -> CmdResult<()> {
    let playlist = Playlist {
        video_id,
        video_ids: queue
            .as_deref()
            .map(|q| {
                q.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        current_index: index,
        current_time: position,
        list_id,
    };
    remote(&state, &screen_id, "set playlist", async move |r| {
        r.set_playlist(&playlist).await
    })
    .await
}

#[tauri::command]
async fn remote_add_video(
    state: State<'_, AppState>,
    screen_id: String,
    video_id: String,
) -> CmdResult<()> {
    remote(&state, &screen_id, "add video", async move |r| {
        r.add_video(&video_id).await
    })
    .await
}

#[tauri::command]
async fn remote_add_videos(
    state: State<'_, AppState>,
    screen_id: String,
    video_ids: String,
) -> CmdResult<()> {
    let ids: Vec<String> = video_ids
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    remote(&state, &screen_id, "add videos", async move |r| {
        r.add_videos(&ids).await
    })
    .await
}

#[tauri::command]
async fn remote_insert_video(
    state: State<'_, AppState>,
    screen_id: String,
    video_id: String,
) -> CmdResult<()> {
    remote(&state, &screen_id, "insert video", async move |r| {
        r.insert_video(&video_id).await
    })
    .await
}

#[tauri::command]
async fn remote_remove_video(
    state: State<'_, AppState>,
    screen_id: String,
    video_id: String,
) -> CmdResult<()> {
    remote(&state, &screen_id, "remove video", async move |r| {
        r.remove_video(&video_id).await
    })
    .await
}

#[tauri::command]
async fn remote_clear_playlist(state: State<'_, AppState>, screen_id: String) -> CmdResult<()> {
    remote(&state, &screen_id, "clear playlist", async |r| {
        r.clear_playlist().await
    })
    .await
}

#[tauri::command]
async fn remote_queue(state: State<'_, AppState>, screen_id: String) -> CmdResult<QueueState> {
    hub(&state)
        .await?
        .queue(&screen_id)
        .ok_or_else(|| format!("unknown device {screen_id}"))
}

#[tauri::command]
async fn remote_queue_items(
    state: State<'_, AppState>,
    screen_id: String,
) -> CmdResult<Vec<QueueItem>> {
    hub(&state).await?.queue_items(&screen_id).await
}

#[tauri::command]
async fn remote_volume(state: State<'_, AppState>, screen_id: String) -> CmdResult<VolumeState> {
    hub(&state)
        .await?
        .volume(&screen_id)
        .ok_or_else(|| format!("unknown device {screen_id}"))
}

#[tauri::command]
async fn remote_quality(state: State<'_, AppState>, screen_id: String) -> CmdResult<QualityState> {
    hub(&state)
        .await?
        .quality(&screen_id)
        .ok_or_else(|| format!("unknown device {screen_id}"))
}

fn main() {
    let tray_mode = std::env::args().any(|a| a == "--tray" || a == "--minimized");
    let data_dir = default_data_dir();
    let log_path = logging::init(&data_dir, false);
    let bus = event_bus();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            data_dir,
            log_path,
            tray_mode,
            bus,
            service: Mutex::new(None),
            discovery: Mutex::new(None),
        })
        .setup(|app| {
            let handle = app.handle().clone();
            let state = app.state::<AppState>();
            let mut events = state.bus.subscribe();
            let data_dir = state.data_dir.clone();

            // Forward service events to the UI and keep the ad counters.
            let emitter = handle.clone();
            tauri::async_runtime::spawn(async move {
                use tokio::sync::broadcast::error::RecvError;
                loop {
                    match events.recv().await {
                        Ok(ev) => {
                            if let ServiceEvent::Ad { action, .. } = &ev {
                                let stats = Stats::record_persistent(&data_dir, *action);
                                let _ = emitter.emit("stats", stats);
                            }
                            let _ = emitter.emit("service-event", ev);
                        }
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => break,
                    }
                }
            });

            // Tray: Show / Start / Stop / Quit. Left-click toggles the window.
            let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
            let start = MenuItem::with_id(app, "start", "Start", true, None::<&str>)?;
            let stop = MenuItem::with_id(app, "stop", "Stop", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &start, &stop, &quit])?;
            TrayIconBuilder::with_id("main")
                .tooltip("Skipjack")
                .icon(app.default_window_icon().cloned().expect("app icon"))
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "start" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let state = app.state::<AppState>();
                            let mut slot = state.service.lock().await;
                            if slot.as_ref().is_some_and(Service::is_running) {
                                return;
                            }
                            let config = match load_from_data_dir(&state.data_dir) {
                                Ok(c) => c,
                                Err(e) => {
                                    tracing::warn!("tray start: config error: {e:#}");
                                    return;
                                }
                            };
                            if config.validate().is_err() {
                                tracing::warn!("tray start: config not usable yet");
                                return;
                            }
                            match Service::start(config, state.bus.clone()) {
                                Ok(s) => {
                                    *slot = Some(s);
                                }
                                Err(e) => tracing::warn!("tray start failed: {e:#}"),
                            }
                        });
                    }
                    "stop" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let state = app.state::<AppState>();
                            let service = { state.service.lock().await.take() };
                            if let Some(service) = service {
                                service.stop().await;
                            }
                        });
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    use tauri::tray::TrayIconEvent;
                    if matches!(event, TrayIconEvent::Click { .. }) {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            if w.is_visible().unwrap_or(true) {
                                let _ = w.hide();
                            } else {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            // In tray mode the window starts hidden and the service stays
            // stopped until the user presses Start (in the window or tray).
            // Otherwise show the window normally.
            if !state.tray_mode {
                if let Some(w) = handle.get_webview_window("main") {
                    let _ = w.show();
                }
            }

            // Auto-start is triggered by the UI (`autostart_service`) once its
            // event listeners exist, otherwise the initial status events are lost.
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window minimizes to tray instead of quitting.
            if window.label() == "main" {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_meta,
            get_stats,
            reset_stats,
            get_config,
            save_config,
            validate_config,
            pair_device,
            start_discovery,
            start_service,
            autostart_service,
            stop_service,
            service_running,
            remote_play,
            remote_pause,
            remote_seek,
            remote_next,
            remote_previous,
            remote_speed,
            remote_subtitles,
            remote_dpad,
            remote_play_video,
            remote_set_playlist,
            remote_add_video,
            remote_add_videos,
            remote_insert_video,
            remote_remove_video,
            remote_clear_playlist,
            remote_queue,
            remote_queue_items,
            remote_volume,
            remote_quality
        ])
        .build(tauri::generate_context!())
        .expect("error while building Skipjack");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            // Leave the TV lounges cleanly.
            let state = handle.state::<AppState>();
            tauri::async_runtime::block_on(async {
                let service = { state.service.lock().await.take() };
                if let Some(service) = service {
                    service.stop().await;
                }
            });
        }
    });
}
