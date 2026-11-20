//! Chromecast / Google Cast discovery (mDNS + Cast v2 protocol).
//!
//! Finds `_googlecast._tcp` devices, opens a Cast v2 channel (TLS on port
//! 8009, length-prefixed protobuf `CastMessage`s) and asks the YouTube
//! receiver app for its MDX screen id.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{sleep_until, Instant};
use tokio_rustls::TlsConnector;

use super::{DiscoveredDevice, DiscoverySource};

const SERVICE_TYPE: &str = "_googlecast._tcp.local.";
const YOUTUBE_APP_ID: &str = "233637DE";
const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
const NS_MDX: &str = "urn:x-cast:com.google.youtube.mdx";
const SENDER: &str = "sender-0";
const RECEIVER: &str = "receiver-0";

// ----------------------------------------------------------------------
// Minimal protobuf codec for CastMessage
// ----------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq)]
struct CastMessage {
    source: String,
    destination: String,
    namespace: String,
    payload: String,
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn put_string(out: &mut Vec<u8>, field: u64, s: &str) {
    put_varint(out, (field << 3) | 2);
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

fn get_varint(data: &[u8], pos: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *data.get(*pos).ok_or_else(|| anyhow!("truncated varint"))?;
        *pos += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
    }
    bail!("varint too long")
}

impl CastMessage {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.payload.len());
        put_varint(&mut out, 1 << 3); // protocol_version = CASTV2_1_0
        put_varint(&mut out, 0);
        put_string(&mut out, 2, &self.source);
        put_string(&mut out, 3, &self.destination);
        put_string(&mut out, 4, &self.namespace);
        put_varint(&mut out, 5 << 3); // payload_type = STRING
        put_varint(&mut out, 0);
        put_string(&mut out, 6, &self.payload);
        out
    }

    fn decode(data: &[u8]) -> Result<Self> {
        let mut msg = Self::default();
        let mut pos = 0;
        while pos < data.len() {
            let key = get_varint(data, &mut pos)?;
            match key & 7 {
                0 => {
                    get_varint(data, &mut pos)?;
                }
                2 => {
                    let len = get_varint(data, &mut pos)? as usize;
                    let end = pos
                        .checked_add(len)
                        .filter(|e| *e <= data.len())
                        .ok_or_else(|| anyhow!("truncated field"))?;
                    let value = String::from_utf8_lossy(&data[pos..end]).into_owned();
                    pos = end;
                    match key >> 3 {
                        2 => msg.source = value,
                        3 => msg.destination = value,
                        4 => msg.namespace = value,
                        6 => msg.payload = value,
                        _ => {} // payload_binary etc.
                    }
                }
                wt => bail!("unsupported wire type {wt}"),
            }
        }
        Ok(msg)
    }
}

// ----------------------------------------------------------------------
// TLS (Cast devices use self-signed certificates)
// ----------------------------------------------------------------------

#[derive(Debug)]
struct AcceptAnyCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn tls_connector() -> Result<TlsConnector> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

// ----------------------------------------------------------------------
// Cast channel
// ----------------------------------------------------------------------

struct CastChannel<S> {
    stream: S,
}

impl<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin> CastChannel<S> {
    async fn send(&mut self, dest: &str, namespace: &str, payload: Value) -> Result<()> {
        let bytes = CastMessage {
            source: SENDER.into(),
            destination: dest.into(),
            namespace: namespace.into(),
            payload: payload.to_string(),
        }
        .encode();
        self.stream
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .await?;
        self.stream.write_all(&bytes).await?;
        self.stream.flush().await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<CastMessage> {
        let mut len = [0u8; 4];
        self.stream.read_exact(&mut len).await?;
        let len = u32::from_be_bytes(len) as usize;
        if len > 1 << 20 {
            bail!("cast message too large ({len} bytes)");
        }
        let mut buf = vec![0u8; len];
        self.stream.read_exact(&mut buf).await?;
        CastMessage::decode(&buf)
    }
}

/// Talks to one Cast device and returns its YouTube screen id.
async fn query_screen_id(ip: IpAddr, port: u16, active: bool) -> Result<Option<String>> {
    let tcp = TcpStream::connect((ip, port)).await?;
    let tls = tls_connector()?
        .connect(ServerName::IpAddress(ip.into()), tcp)
        .await?;
    let mut ch = CastChannel { stream: tls };

    ch.send(RECEIVER, NS_CONNECTION, json!({"type": "CONNECT"}))
        .await?;
    ch.send(
        RECEIVER,
        NS_RECEIVER,
        json!({"type": "GET_STATUS", "requestId": 1}),
    )
    .await?;

    let mut transport: Option<String> = None;
    let mut launched = false;
    loop {
        let msg = ch.recv().await?;
        let Ok(payload) = serde_json::from_str::<Value>(&msg.payload) else {
            continue;
        };
        let kind = payload["type"].as_str().unwrap_or_default();
        match (msg.namespace.as_str(), kind) {
            (NS_HEARTBEAT, "PING") => {
                ch.send(&msg.source, NS_HEARTBEAT, json!({"type": "PONG"}))
                    .await?;
            }
            (NS_RECEIVER, "RECEIVER_STATUS") if transport.is_none() => {
                let app = payload["status"]["applications"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|a| a["appId"] == YOUTUBE_APP_ID);
                match app.and_then(|a| a["transportId"].as_str()) {
                    Some(t) => {
                        let t = t.to_string();
                        ch.send(&t, NS_CONNECTION, json!({"type": "CONNECT"}))
                            .await?;
                        ch.send(&t, NS_MDX, json!({"type": "getMdxSessionStatus"}))
                            .await?;
                        transport = Some(t);
                    }
                    None if !launched && active => {
                        launched = true;
                        ch.send(
                            RECEIVER,
                            NS_RECEIVER,
                            json!({"type": "LAUNCH", "appId": YOUTUBE_APP_ID, "requestId": 2}),
                        )
                        .await?;
                    }
                    None if !launched => return Ok(None),
                    None => {} // still launching
                }
            }
            (NS_RECEIVER, "LAUNCH_ERROR") => return Ok(None),
            (NS_MDX, "mdxSessionStatus") => {
                let screen_id = payload["data"]["screenId"].as_str().map(str::to_string);
                if let Some(t) = &transport {
                    let _ = ch.send(t, NS_CONNECTION, json!({"type": "CLOSE"})).await;
                }
                return Ok(screen_id.filter(|s| !s.is_empty()));
            }
            _ => {}
        }
    }
}

async fn probe_device(
    ip: IpAddr,
    port: u16,
    name: String,
    active: bool,
) -> Result<Option<DiscoveredDevice>> {
    let screen_id = tokio::time::timeout(Duration::from_secs(8), query_screen_id(ip, port, active))
        .await
        .map_err(|_| anyhow!("timed out talking to {name} ({ip})"))??;
    Ok(screen_id.map(|id| DiscoveredDevice::new(id, name, DiscoverySource::Chromecast)))
}

/// Browses mDNS for Cast devices and probes each for a YouTube screen id.
pub async fn discover(active: bool, tx: mpsc::Sender<DiscoveredDevice>) -> Result<()> {
    let mdns = ServiceDaemon::new()?;
    let events = mdns.browse(SERVICE_TYPE)?;
    let deadline = Instant::now()
        + if active {
            Duration::from_secs(5)
        } else {
            Duration::from_secs(15)
        };
    let mut seen = HashSet::new();
    let mut tasks = JoinSet::new();

    loop {
        let browsing = Instant::now() < deadline;
        if !browsing && tasks.is_empty() {
            break;
        }
        tokio::select! {
            ev = events.recv_async(), if browsing => {
                let Ok(ev) = ev else { continue };
                if let ServiceEvent::ServiceResolved(info) = ev {
                    if !seen.insert(info.get_fullname().to_string()) {
                        continue;
                    }
                    let Some(ip) = info.get_addresses_v4().into_iter().next() else { continue };
                    let name = info
                        .get_property_val_str("fn")
                        .map(str::to_string)
                        .unwrap_or_else(|| info.get_fullname().split('.').next().unwrap_or("Chromecast").to_string());
                    tracing::debug!("discovered cast device {name} at {ip}");
                    tasks.spawn(probe_device(IpAddr::V4(ip), info.get_port(), name, active));
                }
            }
            _ = sleep_until(deadline), if browsing => {}
            Some(res) = tasks.join_next(), if !tasks.is_empty() => {
                match res {
                    Ok(Ok(Some(device))) => {
                        if tx.send(device).await.is_err() {
                            break;
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(e)) => tracing::debug!("cast probe failed: {e:#}"),
                    Err(e) => tracing::debug!("cast task failed: {e}"),
                }
            }
            _ = tx.closed() => break,
        }
    }

    let _ = mdns.stop_browse(SERVICE_TYPE);
    let _ = mdns.shutdown();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cast_message_roundtrip() {
        let msg = CastMessage {
            source: SENDER.into(),
            destination: RECEIVER.into(),
            namespace: NS_RECEIVER.into(),
            payload: r#"{"type":"GET_STATUS","requestId":1}"#.into(),
        };
        assert_eq!(CastMessage::decode(&msg.encode()).unwrap(), msg);
    }
}
