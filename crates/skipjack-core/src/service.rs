//! The ad blocking service: one lounge listener per configured device.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::sync::{broadcast, mpsc, Mutex as AsyncMutex};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

use crate::api::build_http_client;
use crate::config::{Config, ConfigError, Device};
use crate::lounge::{
    AdAction, LoungeClient, LoungeError, LoungeEvent, LoungeOptions, PlaybackState, QualityState,
    QueueItem, QueueState, VolumeState,
};

const RETRY_DELAY: Duration = Duration::from_secs(10);
const AUTH_REFRESH_INTERVAL: Duration = Duration::from_secs(60 * 60 * 24);

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum DeviceStatus {
    Authenticating,
    WaitingForDevice,
    Connecting,
    Connected { screen_name: Option<String> },
    Disconnected,
    Error { message: String },
}

/// Events published on the service bus (consumed by the CLI log or the GUI).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ServiceEvent {
    Started {
        devices: usize,
    },
    Warning {
        message: String,
    },
    DeviceStatus {
        screen_id: String,
        name: String,
        status: DeviceStatus,
    },
    Playback {
        screen_id: String,
        name: String,
        playback: PlaybackState,
    },
    Ad {
        screen_id: String,
        name: String,
        action: AdAction,
    },
    Queue {
        screen_id: String,
        name: String,
        queue: QueueState,
    },
    Volume {
        screen_id: String,
        name: String,
        volume: VolumeState,
    },
    Quality {
        screen_id: String,
        name: String,
        quality: QualityState,
    },
    Stopped,
}

pub type EventBus = broadcast::Sender<ServiceEvent>;

pub fn event_bus() -> EventBus {
    broadcast::channel(256).0
}

/// Running service handle.
pub struct Service {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
    remotes: Arc<AsyncMutex<HashMap<String, Arc<LoungeClient>>>>,
}

impl Service {
    /// Validates the config and spawns one listener per device.
    /// Must be called from within a Tokio runtime.
    pub fn start(config: Config, bus: EventBus) -> Result<Self, ConfigError> {
        let warnings = config.validate()?;
        for w in warnings {
            tracing::warn!("{w}");
            let _ = bus.send(ServiceEvent::Warning { message: w });
        }

        let http = build_http_client(config.use_proxy);
        let opts = LoungeOptions {
            join_name: if config.join_name.trim().is_empty() {
                "Skipjack".into()
            } else {
                config.join_name.clone()
            },
            mute_ads: config.mute_ads,
            skip_ads: config.skip_ads,
            auto_play: config.auto_play,
            experimental_reload_ads: config.experimental_reload_ads,
        };
        tracing::info!(
            "options: mute_ads={} skip_ads={} auto_play={} experimental_reload_ads={} use_proxy={} join_name={:?}",
            opts.mute_ads,
            opts.skip_ads,
            opts.auto_play,
            opts.experimental_reload_ads,
            config.use_proxy,
            opts.join_name
        );
        let cancel = CancellationToken::new();
        let remotes: Arc<AsyncMutex<HashMap<String, Arc<LoungeClient>>>> = Default::default();

        let mut set = JoinSet::new();
        for device in config.devices.clone() {
            let ctx = DeviceCtx::new(
                device,
                http.clone(),
                opts.clone(),
                bus.clone(),
                remotes.clone(),
            );
            set.spawn(ctx.run(cancel.child_token()));
        }
        let _ = bus.send(ServiceEvent::Started {
            devices: config.devices.len(),
        });
        tracing::info!("started listening on {} device(s)", config.devices.len());

        let bus_done = bus.clone();
        let handle = tokio::spawn(async move {
            while set.join_next().await.is_some() {}
            let _ = bus_done.send(ServiceEvent::Stopped);
        });
        Ok(Self {
            cancel,
            handle,
            remotes,
        })
    }

    pub fn is_running(&self) -> bool {
        !self.handle.is_finished()
    }

    /// Disconnects from all devices and waits (bounded) for shutdown.
    pub async fn stop(self) {
        self.cancel.cancel();
        let mut handle = self.handle;
        if tokio::time::timeout(Duration::from_secs(8), &mut handle)
            .await
            .is_err()
        {
            handle.abort();
        }
    }

    /// Waits until the service stops on its own (it normally never does).
    pub async fn wait(&mut self) {
        let _ = (&mut self.handle).await;
    }

    /// Handle to the per-device lounge clients.
    ///
    /// Cloning it lets callers send commands without holding the service lock,
    /// so one slow request can never freeze the rest of the app.
    pub fn hub(&self) -> RemoteHub {
        RemoteHub {
            remotes: self.remotes.clone(),
        }
    }
}

/// Remote-control entry point for a running service (cheap to clone).
#[derive(Clone)]
pub struct RemoteHub {
    remotes: Arc<AsyncMutex<HashMap<String, Arc<LoungeClient>>>>,
}

impl RemoteHub {
    /// Looks up the lounge client of a device. The map lock is released before
    /// the caller awaits the actual command.
    pub async fn client(&self, screen_id: &str) -> Result<Arc<LoungeClient>, String> {
        self.remotes
            .lock()
            .await
            .get(screen_id)
            .cloned()
            .ok_or_else(|| format!("unknown device {screen_id} (is the service running with this device configured?)"))
    }

    /// Last known queue snapshot for a device (from `playlistModified`).
    pub fn queue(&self, screen_id: &str) -> Option<QueueState> {
        self.remotes
            .try_lock()
            .ok()?
            .get(screen_id)
            .map(|r| r.queue())
    }

    /// Full queue with titles via `watch_queue_ajax` (needs a `listId`).
    pub async fn queue_items(&self, screen_id: &str) -> Result<Vec<QueueItem>, String> {
        let remote = self.client(screen_id).await?;
        let q = remote.queue();
        let list = q
            .list_id
            .clone()
            .ok_or_else(|| "queue has no list id (titles unavailable)".to_string())?;
        remote
            .watch_queue(&list)
            .await
            .map_err(|e| format!("watch queue failed: {e:#}"))
    }

    /// Last known volume snapshot for a device (from `onVolumeChanged`).
    pub fn volume(&self, screen_id: &str) -> Option<VolumeState> {
        self.remotes
            .try_lock()
            .ok()?
            .get(screen_id)
            .map(|r| r.volume())
    }

    /// Last known quality snapshot for a device (from `onVideoQualityChanged`).
    pub fn quality(&self, screen_id: &str) -> Option<QualityState> {
        self.remotes
            .try_lock()
            .ok()?
            .get(screen_id)
            .map(|r| r.quality())
    }
}

struct DeviceCtx {
    device: Device,
    lounge: Arc<LoungeClient>,
    bus: EventBus,
    events_rx: tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<LoungeEvent>>>,
}

impl DeviceCtx {
    fn new(
        device: Device,
        http: reqwest::Client,
        opts: LoungeOptions,
        bus: EventBus,
        remotes: Arc<AsyncMutex<HashMap<String, Arc<LoungeClient>>>>,
    ) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let lounge = Arc::new(LoungeClient::new(
            http,
            &device.screen_id,
            &device.name,
            opts,
            tx,
        ));
        // Register synchronously so remote commands work right after start.
        // NOTE: `blocking_lock()` would panic here — `Service::start` runs
        // inside the Tauri async runtime. `try_lock` is safe: nothing else
        // can hold the lock yet at construction time.
        match remotes.try_lock() {
            Ok(mut guard) => {
                guard.insert(device.screen_id.clone(), lounge.clone());
            }
            Err(_) => {
                let remotes = remotes.clone();
                let lounge = lounge.clone();
                let screen_id = device.screen_id.clone();
                tokio::spawn(async move {
                    remotes.lock().await.insert(screen_id, lounge);
                });
            }
        }
        Arc::new(Self {
            device,
            lounge,
            bus,
            events_rx: tokio::sync::Mutex::new(Some(rx)),
        })
    }

    fn status(&self, status: DeviceStatus) {
        tracing::debug!(device = %self.device.name, "status: {status:?}");
        let _ = self.bus.send(ServiceEvent::DeviceStatus {
            screen_id: self.device.screen_id.clone(),
            name: self.device.name.clone(),
            status,
        });
    }

    async fn run(self: Arc<Self>, cancel: CancellationToken) {
        let rx = self
            .events_rx
            .lock()
            .await
            .take()
            .expect("device run twice");
        let events = tokio::spawn(self.clone().event_loop(rx));
        let refresher = tokio::spawn(self.clone().refresh_auth_loop());

        tokio::select! {
            _ = cancel.cancelled() => {}
            _ = self.clone().connection_loop() => {}
        }

        let _ = tokio::time::timeout(Duration::from_secs(3), self.lounge.disconnect()).await;
        events.abort();
        refresher.abort();
        self.status(DeviceStatus::Disconnected);
        tracing::info!(device = %self.device.name, "stopped");
    }

    /// Keeps the lounge token fresh.
    async fn refresh_auth_loop(self: Arc<Self>) {
        loop {
            tokio::time::sleep(AUTH_REFRESH_INTERVAL).await;
            if let Err(e) = self.lounge.refresh_auth().await {
                tracing::warn!(device = %self.device.name, "periodic auth refresh failed: {e:#}");
            }
        }
    }

    /// Main loop: authenticate → wait for TV → connect → subscribe, forever.
    async fn connection_loop(self: Arc<Self>) {
        let name = &self.device.name;
        'outer: loop {
            while !self.lounge.linked() {
                self.status(DeviceStatus::Authenticating);
                match self.lounge.refresh_auth().await {
                    Ok(true) => {}
                    Ok(false) => tokio::time::sleep(RETRY_DELAY).await,
                    Err(e) => {
                        tracing::warn!(device = %name, "auth failed: {e:#}");
                        tokio::time::sleep(RETRY_DELAY).await;
                    }
                }
            }

            let mut announced = false;
            loop {
                match self.lounge.is_available().await {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(e) => tracing::warn!(device = %name, "availability check failed: {e:#}"),
                }
                if !self.lounge.linked() {
                    continue 'outer;
                }
                if !announced {
                    tracing::info!(device = %name, "waiting for device to be available (is YouTube open on the TV?)");
                    self.status(DeviceStatus::WaitingForDevice);
                    announced = true;
                }
                tokio::time::sleep(RETRY_DELAY).await;
            }

            self.status(DeviceStatus::Connecting);
            loop {
                match self.lounge.connect().await {
                    Ok(true) => break,
                    Ok(false) => {
                        tracing::info!(device = %name, "connect not accepted yet, retrying")
                    }
                    Err(e)
                        if matches!(
                            e.downcast_ref::<LoungeError>(),
                            Some(LoungeError::ConnectionDenied)
                        ) =>
                    {
                        self.status(DeviceStatus::Error {
                            message: e.to_string(),
                        });
                        tokio::time::sleep(RETRY_DELAY * 6).await;
                    }
                    Err(e) => tracing::warn!(device = %name, "connect failed: {e:#}"),
                }
                if !self.lounge.linked() {
                    continue 'outer;
                }
                tokio::time::sleep(RETRY_DELAY).await;
            }

            let screen_name = self.lounge.screen_name();
            tracing::info!(
                device = %name,
                "connected to {} ({name})",
                screen_name.as_deref().unwrap_or("unknown screen")
            );
            self.status(DeviceStatus::Connected { screen_name });

            match self.lounge.subscribe().await {
                Ok(()) => tracing::debug!(device = %name, "subscription ended"),
                Err(e) => tracing::warn!(device = %name, "subscription ended with error: {e:#}"),
            }
            self.status(DeviceStatus::Disconnected);
            // Avoid hot loops when YouTube drops us immediately.
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// Forwards lounge events to the service bus.
    async fn event_loop(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<LoungeEvent>) {
        while let Some(event) = rx.recv().await {
            let ev = match event {
                LoungeEvent::Playback(playback) => ServiceEvent::Playback {
                    screen_id: self.device.screen_id.clone(),
                    name: self.device.name.clone(),
                    playback,
                },
                LoungeEvent::Ad(action) => ServiceEvent::Ad {
                    screen_id: self.device.screen_id.clone(),
                    name: self.device.name.clone(),
                    action,
                },
                LoungeEvent::Queue(queue) => ServiceEvent::Queue {
                    screen_id: self.device.screen_id.clone(),
                    name: self.device.name.clone(),
                    queue,
                },
                LoungeEvent::Volume(volume) => ServiceEvent::Volume {
                    screen_id: self.device.screen_id.clone(),
                    name: self.device.name.clone(),
                    volume,
                },
                LoungeEvent::Quality(quality) => ServiceEvent::Quality {
                    screen_id: self.device.screen_id.clone(),
                    name: self.device.name.clone(),
                    quality,
                },
            };
            let _ = self.bus.send(ev);
        }
    }
}
