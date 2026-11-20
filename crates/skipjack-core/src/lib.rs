//! Skipjack core: mute and skip YouTube ads on TVs (Smart TVs, Chromecast,
//! game consoles, ...) through the YouTube Lounge API.

pub mod api;
pub mod config;
pub mod constants;
pub mod discovery;
pub mod logging;
pub mod lounge;
pub mod service;
pub mod stats;

pub use api::{build_http_client, pair_with_code, PairedScreen};
pub use config::{Config, ConfigError, Device};
pub use discovery::{discover, DiscoveredDevice, DiscoverySource};
pub use lounge::{
    AdAction, DpadKey, LoungeClient, LoungeOptions, Playlist, QualityState, QueueItem, QueueState,
    VolumeState,
};
pub use service::{event_bus, DeviceStatus, EventBus, RemoteHub, Service, ServiceEvent};
pub use stats::Stats;
