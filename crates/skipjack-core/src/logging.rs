//! Logging setup shared by the CLI and the desktop app.
//!
//! Logs go to stderr (info by default) and to `<data dir>/logs/skipjack.log`
//! (always debug level, so the file is useful for bug reports). The file is
//! rotated to `skipjack.log.1` on start when it grows above 10 MB.
//! `SKIPJACK_LOG` (an `EnvFilter` directive) overrides both levels.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Layer};

const MAX_LOG_SIZE: u64 = 10 * 1024 * 1024;
const NOISY: &str =
    "hyper=info,hyper_util=info,reqwest=info,h2=info,rustls=info,mdns_sd=warn,tao=info,wry=info";

pub fn log_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}

fn filter(level: &str) -> EnvFilter {
    EnvFilter::try_from_env("SKIPJACK_LOG")
        .unwrap_or_else(|_| EnvFilter::new(format!("{level},{NOISY}")))
}

/// Initialises logging. Returns the log file path if file logging works.
pub fn init(data_dir: &Path, debug: bool) -> Option<PathBuf> {
    let stderr = fmt::layer()
        .with_target(false)
        .with_writer(std::io::stderr)
        .with_filter(filter(if debug { "debug" } else { "info" }));

    let (file_layer, path, file_err) = match open_log_file(data_dir) {
        Ok((file, path)) => {
            let layer = fmt::layer()
                .with_ansi(false)
                .with_target(true)
                .with_thread_ids(false)
                .with_writer(Mutex::new(file))
                .with_filter(filter("debug"));
            (Some(layer), Some(path), None)
        }
        Err(e) => (None, None, Some(e)),
    };

    tracing_subscriber::registry()
        .with(stderr)
        .with(file_layer)
        .init();

    match (&path, file_err) {
        (Some(p), _) => tracing::info!(
            "Skipjack {} – logging to {}",
            env!("CARGO_PKG_VERSION"),
            p.display()
        ),
        (None, Some(e)) => tracing::warn!("file logging disabled: {e}"),
        _ => {}
    }
    path
}

fn open_log_file(data_dir: &Path) -> std::io::Result<(std::fs::File, PathBuf)> {
    let dir = log_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("skipjack.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_SIZE) {
        let _ = std::fs::rename(&path, dir.join("skipjack.log.1"));
    }
    let file = OpenOptions::new().create(true).append(true).open(&path)?;
    Ok((file, path))
}
