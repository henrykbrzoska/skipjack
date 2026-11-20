//! HTTP helpers: shared client and lounge pairing.

use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::constants::{LOUNGE_API, USER_AGENT};

const API_TIMEOUT: Duration = Duration::from_secs(15);

/// Builds the shared HTTP client. When `use_proxy` is false, system proxy
/// environment variables are ignored.
pub fn build_http_client(use_proxy: bool) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_secs(30));
    if !use_proxy {
        builder = builder.no_proxy();
    }
    builder.build().expect("failed to build HTTP client")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PairedScreen {
    pub screen_id: String,
    pub name: Option<String>,
    pub lounge_token: Option<String>,
}

fn normalize_pairing_code(code: &str) -> String {
    code.chars().filter(|c| !matches!(c, '-' | ' ')).collect()
}

/// Pairs with a TV using the code shown in "Settings → Link with TV code".
pub async fn pair_with_code(client: &reqwest::Client, code: &str) -> Result<Option<PairedScreen>> {
    let code = normalize_pairing_code(code);
    tracing::debug!("pairing with code ({} digits)", code.len());
    let resp = client
        .post(format!("{LOUNGE_API}/pairing/get_screen"))
        .form(&[("pairing_code", code.as_str())])
        .timeout(API_TIMEOUT)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        tracing::warn!("pairing failed: HTTP {status}");
        return Ok(None);
    }
    let Ok(body) = resp.json::<Value>().await else {
        tracing::warn!("pairing failed: invalid JSON response");
        return Ok(None);
    };
    let screen = &body["screen"];
    let Some(screen_id) = screen["screenId"].as_str().filter(|s| !s.is_empty()) else {
        tracing::warn!("pairing failed: no screen id in response");
        return Ok(None);
    };
    Ok(Some(PairedScreen {
        screen_id: screen_id.to_string(),
        name: screen["name"].as_str().map(str::to_string),
        lounge_token: screen["loungeToken"].as_str().map(str::to_string),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_codes() {
        assert_eq!(normalize_pairing_code("123-456 789 012"), "123456789012");
    }
}
