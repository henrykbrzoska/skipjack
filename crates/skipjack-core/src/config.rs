//! Configuration file handling.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const CONFIG_FILE_NAME: &str = "config.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Device {
    pub screen_id: String,
    #[serde(default = "default_device_name")]
    pub name: String,
    /// Unknown per-device keys (e.g. the legacy `offset`) are kept.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn default_device_name() -> String {
    "YouTube on TV".into()
}

impl Device {
    pub fn new(screen_id: String, name: String) -> Self {
        Self {
            screen_id,
            name,
            extra: Map::new(),
        }
    }

    /// Old style (26 char) screen ids are being revoked by YouTube.
    pub fn has_legacy_screen_id(&self) -> bool {
        self.screen_id.len() == 26
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub devices: Vec<Device>,
    pub mute_ads: bool,
    pub skip_ads: bool,
    pub auto_play: bool,
    pub join_name: String,
    pub use_proxy: bool,
    /// Experimental: try a `setPlaylist` reload on non-skippable ads.
    /// Off by default; see docs/PLAN-ad-reload.md.
    pub experimental_reload_ads: bool,
    /// Unknown keys are kept so saving never destroys data we don't understand.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            devices: Vec::new(),
            mute_ads: true,
            skip_ads: true,
            auto_play: true,
            join_name: "Skipjack".into(),
            use_proxy: false,
            experimental_reload_ads: false,
            extra: Map::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("the legacy `atvs` option is no longer supported")]
    LegacyAtvs,
    #[error("no devices configured, pair at least one device first")]
    NoDevices,
    #[error("device #{0} has no screen id")]
    MissingScreenId(usize),
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid config file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

impl Config {
    /// Validates the config before starting the service. Returns warnings
    /// that should be shown to the user but don't prevent starting.
    pub fn validate(&self) -> Result<Vec<String>, ConfigError> {
        let mut warnings = Vec::new();
        if self.extra.contains_key("atvs") {
            return Err(ConfigError::LegacyAtvs);
        }
        if self.devices.is_empty() {
            return Err(ConfigError::NoDevices);
        }
        for (i, d) in self.devices.iter().enumerate() {
            if d.screen_id.trim().is_empty() {
                return Err(ConfigError::MissingScreenId(i));
            }
            if d.has_legacy_screen_id() {
                warnings.push(format!(
                    "The screen id of '{}' uses the old 26 character format which YouTube is revoking. \
                     Pair the device again.",
                    d.name
                ));
            }
        }
        if !self.mute_ads && !self.skip_ads {
            warnings.push(
                "Both mute_ads and skip_ads are disabled, Skipjack won't do anything about ads"
                    .into(),
            );
        }
        if self.experimental_reload_ads {
            warnings.push("experimental_reload_ads is enabled: Skipjack will try to escape non-skippable ads (BACK + return to the video, see docs/PLAN-ad-reload.md)".into());
        }
        Ok(warnings)
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Loads the config or returns defaults if the file does not exist.
    pub fn load_or_default(path: &Path) -> Result<Self, ConfigError> {
        match Self::load(path) {
            Err(ConfigError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(Self::default())
            }
            other => other,
        }
    }

    /// Atomically writes the config (write to temp file + rename).
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let io_err = |source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(io_err)?;
        std::fs::rename(&tmp, path).map_err(io_err)
    }
}

/// Default data directory: `$SKIPJACK_DATA_DIR`,
/// then the platform data dir (e.g. `~/.local/share/skipjack`).
pub fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SKIPJACK_DATA_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("skipjack")
}

pub fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CONFIG_FILE_NAME)
}

/// Loads the config from `data_dir`.
pub fn load_from_data_dir(data_dir: &Path) -> Result<Config, ConfigError> {
    let path = config_path(data_dir);
    Config::load_or_default(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legacy_config_and_keeps_unknown_keys() {
        let json = r#"{
            "devices": [{"screen_id": "abc", "name": "TV", "offset": 250}],
            "skip_categories": ["sponsor", "intro"],
            "mute_ads": true,
            "skip_ads": false,
            "auto_play": true,
            "join_name": "Skipjack",
            "apikey": "",
            "use_proxy": false
        }"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert!(cfg.mute_ads && !cfg.skip_ads);
        assert!(!cfg.experimental_reload_ads); // off by default
        assert!(cfg.validate().unwrap().is_empty());
        let back = serde_json::to_value(&cfg).unwrap();
        assert_eq!(back["skip_categories"][1], "intro");
        assert_eq!(back["devices"][0]["offset"], 250);
    }

    #[test]
    fn experimental_reload_ads_is_preserved_and_warned() {
        let cfg: Config = serde_json::from_str(
            r#"{"devices":[{"screen_id":"abc"}],"experimental_reload_ads": true}"#,
        )
        .unwrap();
        assert!(cfg.experimental_reload_ads);
        assert!(cfg
            .validate()
            .unwrap()
            .iter()
            .any(|w| w.contains("experimental_reload_ads")));
        let back = serde_json::to_value(&cfg).unwrap();
        assert_eq!(back["experimental_reload_ads"], true);
    }

    #[test]
    fn rejects_atvs() {
        let cfg: Config = serde_json::from_str(r#"{"atvs": []}"#).unwrap();
        assert!(matches!(cfg.validate(), Err(ConfigError::LegacyAtvs)));
    }
}
