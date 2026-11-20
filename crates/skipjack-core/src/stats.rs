//! Persistent ad counters (`<data dir>/stats.json`), shared by CLI and GUI.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::lounge::AdAction;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Stats {
    pub ads_muted: u64,
    pub ads_skipped: u64,
}

pub fn stats_path(data_dir: &Path) -> PathBuf {
    data_dir.join("stats.json")
}

impl Stats {
    pub fn load(data_dir: &Path) -> Self {
        std::fs::read_to_string(stats_path(data_dir))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) {
        let path = stats_path(data_dir);
        let tmp = path.with_extension("json.tmp");
        let res = std::fs::create_dir_all(data_dir)
            .and_then(|_| std::fs::write(&tmp, serde_json::to_string(self).unwrap_or_default()))
            .and_then(|_| std::fs::rename(&tmp, &path));
        if let Err(e) = res {
            tracing::warn!("failed to save stats to {}: {e}", path.display());
        }
    }

    /// Counts an ad action. Returns true if a counter changed.
    pub fn record(&mut self, action: AdAction) -> bool {
        match action {
            AdAction::Muted => self.ads_muted += 1,
            AdAction::Skipped => self.ads_skipped += 1,
            AdAction::Unmuted => return false,
        }
        true
    }

    /// Loads, records and saves in one go (re-reading the file so the CLI
    /// and the desktop app don't overwrite each other's counts).
    pub fn record_persistent(data_dir: &Path, action: AdAction) -> Self {
        let mut stats = Self::load(data_dir);
        if stats.record(action) {
            stats.save(data_dir);
        }
        stats
    }
}
