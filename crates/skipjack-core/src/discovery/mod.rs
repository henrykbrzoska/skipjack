//! Local network discovery of YouTube TVs (DIAL + Chromecast).

pub mod chromecast;
pub mod dial;

use std::collections::HashSet;

use serde::Serialize;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoverySource {
    Dial,
    Chromecast,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DiscoveredDevice {
    pub screen_id: String,
    pub name: String,
    pub source: DiscoverySource,
}

impl DiscoveredDevice {
    pub fn new(screen_id: String, name: String, source: DiscoverySource) -> Self {
        Self {
            screen_id,
            name,
            source,
        }
    }
}

/// Starts DIAL and Chromecast discovery and yields unique devices as they
/// are found. In active mode the channel closes once discovery finishes;
/// dropping the receiver stops discovery early.
pub fn discover(api_client: reqwest::Client, active: bool) -> mpsc::Receiver<DiscoveredDevice> {
    let (raw_tx, mut raw_rx) = mpsc::channel::<DiscoveredDevice>(16);
    let (out_tx, out_rx) = mpsc::channel::<DiscoveredDevice>(16);

    let dial_tx = raw_tx.clone();
    tokio::spawn(async move {
        if let Err(e) = dial::discover(api_client, active, dial_tx).await {
            tracing::warn!("DIAL discovery failed: {e:#}");
        }
    });
    tokio::spawn(async move {
        if let Err(e) = chromecast::discover(active, raw_tx).await {
            tracing::warn!("Chromecast discovery failed: {e:#}");
        }
    });
    tokio::spawn(async move {
        let mut seen = HashSet::new();
        loop {
            tokio::select! {
                dev = raw_rx.recv() => {
                    let Some(dev) = dev else { break };
                    if seen.insert(dev.screen_id.clone()) && out_tx.send(dev).await.is_err() {
                        break;
                    }
                }
                _ = out_tx.closed() => break,
            }
        }
    });
    out_rx
}
