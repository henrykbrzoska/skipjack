//! `skipjack` – headless daemon and command line setup.

mod setup;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use skipjack_core::config::{config_path, default_data_dir, load_from_data_dir};
use skipjack_core::{
    build_http_client, discover, event_bus, logging, pair_with_code, Device, DpadKey, LoungeClient,
    LoungeOptions, Playlist, Service, ServiceEvent, Stats,
};

#[derive(Parser)]
#[command(
    name = "skipjack",
    version,
    about = "Mute & skip ads in YouTube TV apps"
)]
struct Cli {
    /// Data directory containing config.json (logs go to <data>/logs)
    #[arg(short, long, global = true, env = "SKIPJACK_DATA_DIR")]
    data: Option<PathBuf>,
    /// Verbose (debug) logging on the console; the log file is always verbose
    #[arg(long, global = true)]
    debug: bool,
    /// Experimental: try a setPlaylist reload on non-skippable ads
    #[arg(long, global = true)]
    try_reload: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start blocking ads (default)
    Start,
    /// Interactive command line setup
    Setup,
    /// Find YouTube TVs on the local network
    Discover {
        /// Only query, never launch the YouTube app on devices
        #[arg(long)]
        passive: bool,
    },
    /// Pair with a TV using the "Link with TV code"
    Pair { code: String },
    /// Print the config file path
    ConfigPath,
    /// Print the log file path
    LogPath,
    /// Remote control: send a command to a device (one-shot, no service needed)
    Remote {
        /// Device name or screen_id (prefix match on name, case-insensitive)
        device: String,
        #[command(subcommand)]
        command: RemoteCommand,
    },
}

#[derive(Subcommand)]
enum RemoteCommand {
    /// Resume playback
    Play,
    /// Pause playback
    Pause,
    /// Seek to a position in seconds
    Seek { secs: f64 },
    /// Next item in the queue
    Next,
    /// Previous item in the queue
    Previous,
    /// Set playback speed (e.g. 1.5)
    Speed { speed: f64 },
    /// Set subtitles track (needs current video id)
    Subtitles {
        video_id: String,
        language_code: String,
    },
    /// Send a D-pad key (UP/DOWN/LEFT/RIGHT/ENTER/BACK)
    Dpad { key: String },
    /// Play a video by id
    PlayVideo { video_id: String },
    /// Load a video + queue (comma-separated ids, first plays)
    SetPlaylist {
        video_id: String,
        #[arg(long)]
        queue: Option<String>,
        #[arg(long)]
        index: Option<usize>,
        #[arg(long)]
        position: Option<f64>,
        #[arg(long)]
        list_id: Option<String>,
    },
    /// Append a video at the end of the queue
    AddVideo { video_id: String },
    /// Append several videos (comma-separated, one request every 2–5 s)
    AddVideos { video_ids: String },
    /// Insert a video right after the currently playing one
    InsertVideo { video_id: String },
    /// Remove a video from the queue
    RemoveVideo { video_id: String },
    /// Clear the queue
    ClearPlaylist,
    /// Show the current queue (snapshot from bind + titles via watch_queue_ajax)
    Queue,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let data_dir = cli.data.unwrap_or_else(default_data_dir);
    let command = cli.command.unwrap_or(Command::Start);
    // Keep interactive commands quiet on the console unless --debug.
    let console_debug = cli.debug;
    match command {
        Command::ConfigPath => {
            println!("{}", config_path(&data_dir).display());
            return Ok(());
        }
        Command::LogPath => {
            println!(
                "{}",
                logging::log_dir(&data_dir).join("skipjack.log").display()
            );
            return Ok(());
        }
        _ => {
            logging::init(&data_dir, console_debug);
        }
    }

    match command {
        Command::Start => start(data_dir, cli.try_reload).await,
        Command::Setup => setup::run(&data_dir).await,
        Command::Discover { passive } => {
            let cfg = load_from_data_dir(&data_dir)?;
            println!("Searching for devices (this takes a few seconds)...");
            let mut rx = discover(build_http_client(cfg.use_proxy), !passive);
            let mut found = 0;
            while let Some(d) = rx.recv().await {
                found += 1;
                println!("{:<30} {:?}  screen_id={}", d.name, d.source, d.screen_id);
            }
            println!("Found {found} device(s)");
            Ok(())
        }
        Command::Pair { code } => {
            let mut cfg = load_from_data_dir(&data_dir)?;
            let client = build_http_client(cfg.use_proxy);
            let paired = pair_with_code(&client, &code)
                .await?
                .context("failed to pair device, try restarting the YouTube app")?;
            let name = paired
                .name
                .clone()
                .unwrap_or_else(|| "YouTube on TV".into());
            cfg.devices.retain(|d| d.screen_id != paired.screen_id);
            cfg.devices
                .push(Device::new(paired.screen_id, name.clone()));
            cfg.save(&config_path(&data_dir))?;
            println!("Paired device: {name}");
            Ok(())
        }
        Command::ConfigPath | Command::LogPath => unreachable!(),
        Command::Remote { device, command } => remote(data_dir, device, command).await,
    }
}

/// One-shot remote: authenticate → connect → send one command → disconnect.
async fn remote(data_dir: PathBuf, device: String, command: RemoteCommand) -> Result<()> {
    let cfg = load_from_data_dir(&data_dir)?;
    let needle = device.to_lowercase();
    let dev = cfg
        .devices
        .iter()
        .find(|d| d.screen_id == device || d.name.to_lowercase().starts_with(&needle))
        .with_context(|| format!("unknown device {device:?} (check `skipjack setup` for names)"))?;
    let key = match &command {
        RemoteCommand::Dpad { key } => Some(DpadKey::parse(key).with_context(|| {
            format!("bad dpad key {key:?}, want UP/DOWN/LEFT/RIGHT/ENTER/BACK")
        })?),
        _ => None,
    };

    let http = build_http_client(cfg.use_proxy);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let opts = LoungeOptions {
        join_name: if cfg.join_name.trim().is_empty() {
            "Skipjack".into()
        } else {
            cfg.join_name.clone()
        },
        mute_ads: false,
        skip_ads: false,
        auto_play: false,
        experimental_reload_ads: false,
    };
    let lounge = std::sync::Arc::new(LoungeClient::new(http, &dev.screen_id, &dev.name, opts, tx));
    anyhow::ensure!(lounge.refresh_auth().await?, "authentication failed");
    anyhow::ensure!(
        lounge.connect().await?,
        "connect failed (is YouTube open on the TV?)"
    );
    let ok = match &command {
        RemoteCommand::Play => lounge.play().await?,
        RemoteCommand::Pause => lounge.pause().await?,
        RemoteCommand::Seek { secs } => lounge.seek_to(*secs).await?,
        RemoteCommand::Next => lounge.next().await?,
        RemoteCommand::Previous => lounge.previous().await?,
        RemoteCommand::Speed { speed } => lounge.set_speed(*speed).await?,
        RemoteCommand::Subtitles {
            video_id,
            language_code,
        } => lounge.set_subtitles(video_id, language_code).await?,
        RemoteCommand::Dpad { .. } => lounge.dpad(key.unwrap()).await?,
        RemoteCommand::PlayVideo { video_id } => lounge.play_video(video_id).await?,
        RemoteCommand::SetPlaylist {
            video_id,
            queue,
            index,
            position,
            list_id,
        } => {
            let ok = lounge
                .set_playlist(&Playlist {
                    video_id: video_id.clone(),
                    video_ids: queue
                        .as_deref()
                        .map(|q| {
                            q.split(',')
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect()
                        })
                        .unwrap_or_default(),
                    current_index: *index,
                    current_time: *position,
                    list_id: list_id.clone(),
                })
                .await?;
            // `setPlaylist` only cues the video; the TV waits for `play`.
            if ok {
                lounge.start_playback().await?;
            }
            ok
        }
        RemoteCommand::AddVideo { video_id } => lounge.add_video(video_id).await?,
        RemoteCommand::AddVideos { video_ids } => {
            let ids: Vec<String> = video_ids
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            lounge.add_videos(&ids).await?
        }
        RemoteCommand::InsertVideo { video_id } => lounge.insert_video(video_id).await?,
        RemoteCommand::RemoveVideo { video_id } => lounge.remove_video(video_id).await?,
        RemoteCommand::ClearPlaylist => lounge.clear_playlist().await?,
        RemoteCommand::Queue => {
            // The bind frame already carried `playlistModified` → snapshot.
            let q = lounge.queue();
            match &q.list_id {
                Some(list) => {
                    println!("queue: {} item(s), list={list}", q.video_ids.len());
                    match lounge.watch_queue(list).await {
                        Ok(items) => {
                            for (i, it) in items.iter().enumerate() {
                                println!(
                                    "  {}. {}  {}",
                                    i + 1,
                                    it.video_id,
                                    it.title.as_deref().unwrap_or("?")
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!("watch_queue_ajax failed ({e:#}), falling back to ids");
                            for (i, id) in q.video_ids.iter().enumerate() {
                                println!("  {}. {id}", i + 1);
                            }
                        }
                    }
                }
                None => {
                    println!(
                        "queue: {} item(s) (no list id, titles unavailable)",
                        q.video_ids.len()
                    );
                    for (i, id) in q.video_ids.iter().enumerate() {
                        println!("  {}. {id}", i + 1);
                    }
                }
            }
            let _ = lounge.disconnect().await;
            return Ok(());
        }
    };
    let _ = lounge.disconnect().await;
    anyhow::ensure!(
        ok,
        "command rejected (session lost, is YouTube open on the TV?)"
    );
    println!("ok");
    Ok(())
}

async fn start(data_dir: PathBuf, try_reload: bool) -> Result<()> {
    let mut config = load_from_data_dir(&data_dir)?;
    if try_reload {
        config.experimental_reload_ads = true;
        tracing::warn!("--try-reload: experimental ad reload enabled for this run");
    }
    tracing::info!("config: {}", config_path(&data_dir).display());
    let bus = event_bus();
    let mut events = bus.subscribe();
    let stats_dir = data_dir.clone();
    tokio::spawn(async move {
        use tokio::sync::broadcast::error::RecvError;
        loop {
            match events.recv().await {
                Ok(ServiceEvent::Ad { action, name, .. }) => {
                    let s = Stats::record_persistent(&stats_dir, action);
                    tracing::info!(device = %name, "ad {action:?} (total: {} muted, {} skipped)", s.ads_muted, s.ads_skipped);
                }
                Ok(ServiceEvent::Queue { name, queue, .. }) => {
                    tracing::info!(device = %name, "queue: {} item(s){}", queue.video_ids.len(), queue.list_id.as_deref().map(|l| format!(" list={l}")).unwrap_or_default());
                }
                Ok(ServiceEvent::Volume { name, volume, .. }) => {
                    tracing::info!(device = %name, "volume: {}{}", volume.volume, if volume.muted { " (muted)" } else { "" });
                }
                Ok(ServiceEvent::Quality { name, quality, .. }) => {
                    tracing::info!(device = %name, "quality: {}", quality.level.as_deref().unwrap_or("?"));
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });
    let service = Service::start(config, bus).with_context(|| {
        format!(
            "invalid configuration (run `skipjack setup`), config: {}",
            config_path(&data_dir).display()
        )
    })?;

    shutdown_signal().await;
    tracing::info!("cancelling tasks and exiting...");
    service.stop().await;
    tracing::info!("exited");
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
