//! DIAL (SSDP M-SEARCH) discovery of YouTube capable TVs.
//!
//! The DIAL DIscovery And Launch protocol specification is © Netflix,
//! see www.dial-multiscreen.org.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use anyhow::{anyhow, Result};
use rand::Rng;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{sleep_until, Instant};

use super::{DiscoveredDevice, DiscoverySource};
use crate::api::pair_with_code;

const SSDP_ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(239, 255, 255, 250)), 1900);
const SEARCH_TARGET: &str = "urn:dial-multiscreen-org:service:dial:1";
const ACTIVE_LISTEN: Duration = Duration::from_secs(4);
const PASSIVE_POLL: Duration = Duration::from_secs(15);

/// Local IP used for multicast (needed on Windows / multi-homed hosts).
pub fn local_ipv4() -> Ipv4Addr {
    let probe = || -> Option<Ipv4Addr> {
        let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        s.connect("10.254.254.254:1").ok()?;
        match s.local_addr().ok()?.ip() {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(_) => None,
        }
    };
    probe().unwrap_or(Ipv4Addr::UNSPECIFIED)
}

fn search_request() -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: {SSDP_ADDR}\r\nMAN: \"ssdp:discover\"\r\nMX: 3\r\nST: {SEARCH_TARGET}\r\n\r\n"
    )
}

fn location_header(response: &str) -> Option<String> {
    response.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("location")
            .then(|| v.trim().to_string())
    })
}

/// Only trust DIAL URLs that point back to the device that answered.
fn url_matches_ip(url: &str, expected: IpAddr) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    match parsed.host() {
        Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip) == expected,
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip) == expected,
        Some(url::Host::Domain(d)) => d.parse::<IpAddr>().is_ok_and(|ip| ip == expected),
        None => false,
    }
}

fn xml_text(xml: &str, tag: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    doc.descendants()
        .find(|n| n.is_element() && n.tag_name().name() == tag)
        .and_then(|n| n.text())
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

fn random_pairing_code() -> String {
    let mut rng = rand::thread_rng();
    (0..12)
        .map(|_| char::from(b'0' + rng.gen_range(0..10u8)))
        .collect()
}

/// Queries a DIAL device for its YouTube screen id. In active mode the
/// YouTube app is launched with a generated pairing code when needed.
async fn find_youtube_app(
    api_client: reqwest::Client,
    dial_client: reqwest::Client,
    location: String,
    sender: IpAddr,
    active: bool,
) -> Result<Option<DiscoveredDevice>> {
    if !url_matches_ip(&location, sender) {
        tracing::debug!("ignoring DIAL location {location} from {sender}");
        return Ok(None);
    }
    let resp = dial_client.get(&location).send().await?;
    let app_url = resp
        .headers()
        .get("application-url")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("no Application-URL header"))?;
    let description = resp.text().await?;
    let name = xml_text(&description, "friendlyName").unwrap_or_else(|| sender.to_string());
    if !url_matches_ip(&app_url, sender) {
        tracing::debug!("ignoring DIAL application url {app_url} from {sender}");
        return Ok(None);
    }

    let youtube_url = format!("{}/YouTube", app_url.trim_end_matches('/'));
    if let Ok(resp) = dial_client
        .get(&youtube_url)
        .header("Origin", "https://www.youtube.com")
        .send()
        .await
    {
        if let Ok(xml) = resp.text().await {
            if let Some(screen_id) = xml_text(&xml, "screenId") {
                return Ok(Some(DiscoveredDevice::new(
                    screen_id,
                    name,
                    DiscoverySource::Dial,
                )));
            }
        }
    }
    if !active {
        return Ok(None);
    }

    // Launch YouTube with a pairing code we generate, then pair with it.
    let code = random_pairing_code();
    tracing::debug!("launching YouTube on {name}");
    let resp = dial_client
        .post(&youtube_url)
        .header("Origin", "https://www.youtube.com")
        .form(&[("pairingCode", code.as_str()), ("theme", "cl")])
        .send()
        .await?;
    if resp.headers().get("location").is_none() {
        return Ok(None);
    }
    for _ in 0..3 {
        if let Ok(Some(paired)) = pair_with_code(&api_client, &code).await {
            return Ok(Some(DiscoveredDevice::new(
                paired.screen_id,
                name,
                DiscoverySource::Dial,
            )));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Ok(None)
}

/// Runs DIAL discovery, sending found devices to `tx`. In active mode this
/// returns after one discovery cycle; in passive mode it polls until `tx`
/// is closed.
pub async fn discover(
    api_client: reqwest::Client,
    active: bool,
    tx: mpsc::Sender<DiscoveredDevice>,
) -> Result<()> {
    let dial_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()?;

    let socket = UdpSocket::bind(SocketAddr::new(IpAddr::V4(local_ipv4()), 0)).await?;
    let request = search_request();
    socket.send_to(request.as_bytes(), SSDP_ADDR).await?;

    let listen_until = if active {
        Instant::now() + ACTIVE_LISTEN
    } else {
        Instant::now() + Duration::from_secs(60 * 60 * 24 * 365)
    };
    let mut next_poll = Instant::now() + PASSIVE_POLL;
    let mut locations = HashSet::new();
    let mut tasks = JoinSet::new();
    let mut buf = vec![0u8; 4096];

    loop {
        let listening = Instant::now() < listen_until;
        if !listening && tasks.is_empty() {
            break;
        }
        let wake = if active { listen_until } else { next_poll };
        tokio::select! {
            res = socket.recv_from(&mut buf), if listening => {
                let Ok((n, from)) = res else { continue };
                let text = String::from_utf8_lossy(&buf[..n]);
                if let Some(location) = location_header(&text) {
                    if locations.insert(location.clone()) {
                        tracing::debug!("DIAL device at {location}");
                        tasks.spawn(find_youtube_app(api_client.clone(), dial_client.clone(), location, from.ip(), active));
                    }
                }
            }
            _ = sleep_until(wake), if listening => {
                if !active {
                    tracing::debug!("sending periodic M-SEARCH");
                    let _ = socket.send_to(request.as_bytes(), SSDP_ADDR).await;
                    next_poll = Instant::now() + PASSIVE_POLL;
                }
            }
            Some(res) = tasks.join_next(), if !tasks.is_empty() => {
                match res {
                    Ok(Ok(Some(device))) => {
                        if tx.send(device).await.is_err() {
                            return Ok(());
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(e)) => tracing::debug!("DIAL device query failed: {e:#}"),
                    Err(e) => tracing::debug!("DIAL task failed: {e}"),
                }
            }
            _ = tx.closed() => return Ok(()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssdp_and_xml() {
        let resp = "HTTP/1.1 200 OK\r\nLOCATION: http://192.168.1.5:8008/ssdp/device-desc.xml\r\nST: x\r\n\r\n";
        assert_eq!(
            location_header(resp).as_deref(),
            Some("http://192.168.1.5:8008/ssdp/device-desc.xml")
        );
        let ip: IpAddr = "192.168.1.5".parse().unwrap();
        assert!(url_matches_ip("http://192.168.1.5:8008/apps", ip));
        assert!(!url_matches_ip("http://192.168.1.6:8008/apps", ip));
        assert!(!url_matches_ip("http://evil.example/apps", ip));

        let xml = r#"<?xml version="1.0"?><service xmlns="urn:dial-multiscreen-org:schemas:dial">
            <name>YouTube</name><additionalData><screenId>abc123</screenId></additionalData></service>"#;
        assert_eq!(xml_text(xml, "screenId").as_deref(), Some("abc123"));
        assert_eq!(random_pairing_code().len(), 12);
    }
}
