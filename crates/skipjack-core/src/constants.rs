//! Static values shared by the whole engine.

pub const APP_NAME: &str = "Skipjack";
pub const USER_AGENT: &str = concat!("Skipjack/", env!("CARGO_PKG_VERSION"));

pub const LOUNGE_API: &str = "https://www.youtube.com/api/lounge";

/// App identifier the lounge bind endpoint expects (mirrors the iOS app).
pub const LOUNGE_APP: &str = "ytios-phone-20.15.1";

/// YouTube clients that break when a remote joins the lounge.
pub const YOUTUBE_CLIENT_BLACKLIST: &[&str] = &["TVHTML5_FOR_KIDS"];
