//! A small Electrum client: JSON-RPC lines over TLS, one connection per server, requests
//! pipelined on it so several can be in flight at once.
//!
//! Most public Electrum servers use self-signed certificates. Like Electrum itself, a certificate
//! is trusted the first time it is seen and pinned by its SHA-256 after that; one signed by a
//! public CA is accepted as usual. A pin that changes is refused, since that is what an attacker
//! in the middle looks like.

use super::{Hist, NetError};
use bdk_wallet::bitcoin::hashes::{Hash, sha256};
use bdk_wallet::bitcoin::{BlockHash, Script, Transaction, Txid, block::Header, consensus};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

const TIMEOUT: Duration = Duration::from_secs(25);

/// Certificate pins, `host:port` to the hex SHA-256 of the leaf certificate.
pub type Pins = Arc<Mutex<HashMap<String, String>>>;

type Waiting = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

pub struct Electrum {
    url: String,
    writer: AsyncMutex<tokio::io::WriteHalf<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>>,
    waiting: Waiting,
    next: Mutex<u64>,
}

impl Electrum {
    pub async fn connect(url: &str, pins: Pins) -> Result<Electrum, NetError> {
        let fail = |why: String| NetError::Down(format!("{url}: {why}"));
        let (host, _) = url.rsplit_once(':').ok_or_else(|| fail("no port".into()))?;
        let verifier = Arc::new(Pinning::new(url.to_string(), pins));
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| fail(e.to_string()))?
            .dangerous()
            .with_custom_certificate_verifier(verifier.clone())
            .with_no_client_auth();
        let name = ServerName::try_from(host.to_string()).map_err(|e| fail(e.to_string()))?;
        let tcp = tokio::time::timeout(TIMEOUT, tokio::net::TcpStream::connect(url))
            .await
            .map_err(|_| fail("did not answer in time".into()))?
            .map_err(|e| fail(e.to_string()))?;
        let tls = tokio::time::timeout(TIMEOUT, tokio_rustls::TlsConnector::from(Arc::new(config)).connect(name, tcp))
            .await
            .map_err(|_| fail("TLS did not finish in time".into()))?
            .map_err(|e| match verifier.refused() {
                Some(why) => NetError::Untrusted(format!("{url}: {why}")),
                None => fail(format!("TLS failed: {e}")),
            })?;
        let (read, write) = tokio::io::split(tls);
        let waiting: Waiting = Arc::default();
        let w = waiting.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                // Notifications have no id; this client does not subscribe.
                let Some(id) = v.get("id").and_then(Value::as_u64) else { continue };
                let Some(tx) = w.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) else { continue };
                let r = match v.get("error").filter(|e| !e.is_null()) {
                    Some(e) => Err(e.get("message").and_then(Value::as_str).unwrap_or("error").to_string()),
                    None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(r);
            }
            // Connection gone: wake everyone still waiting.
            w.lock().unwrap_or_else(|e| e.into_inner()).clear();
        });
        let e = Electrum { url: url.to_string(), writer: AsyncMutex::new(write), waiting, next: Mutex::new(0) };
        e.call("server.version", json!(["coldbench", "1.4"])).await?;
        Ok(e)
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, NetError> {
        let id = {
            let mut n = self.next.lock().unwrap_or_else(|e| e.into_inner());
            *n += 1;
            *n
        };
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tx);
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string() + "\n";
        {
            let mut w = self.writer.lock().await;
            w.write_all(line.as_bytes()).await.map_err(|e| NetError::Down(format!("{}: {e}", self.url)))?;
        }
        match tokio::time::timeout(TIMEOUT, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(m))) => Err(NetError::Refused(format!("{}: {m}", self.url))),
            Ok(Err(_)) => Err(NetError::Down(format!("{}: the connection closed", self.url))),
            Err(_) => Err(NetError::Down(format!("{}: did not answer in time", self.url))),
        }
    }

    pub async fn history(&self, spk: &Script) -> Result<Vec<Hist>, NetError> {
        let mut h = sha256::Hash::hash(spk.as_bytes()).to_byte_array();
        h.reverse();
        let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
        let v = self.call("blockchain.scripthash.get_history", json!([hex])).await?;
        let rows = v.as_array().ok_or_else(|| self.bad("history is not a list"))?;
        rows.iter()
            .map(|r| {
                let txid: Txid = r.get("tx_hash").and_then(Value::as_str).and_then(|s| s.parse().ok()).ok_or_else(|| self.bad("a history row without a txid"))?;
                let height = r.get("height").and_then(Value::as_i64).unwrap_or(0);
                Ok(Hist { txid, height: (height > 0).then_some(height as u32) })
            })
            .collect()
    }

    pub async fn tx(&self, txid: Txid) -> Result<Transaction, NetError> {
        let v = self.call("blockchain.transaction.get", json!([txid.to_string(), false])).await?;
        let hex = v.as_str().ok_or_else(|| self.bad("transaction is not hex"))?;
        let tx: Transaction = consensus::encode::deserialize_hex(hex).map_err(|e| self.bad(&e.to_string()))?;
        if tx.compute_txid() != txid {
            return Err(NetError::Lied(format!("{}: sent a different transaction than asked for", self.url)));
        }
        Ok(tx)
    }

    /// The merkle branch of `txid` in the block at `height`, and its position.
    pub async fn proof(&self, txid: Txid, height: u32) -> Result<(Vec<Txid>, u32), NetError> {
        let v = self.call("blockchain.transaction.get_merkle", json!([txid.to_string(), height])).await?;
        let pos = v.get("pos").and_then(Value::as_u64).ok_or_else(|| self.bad("a proof without a position"))? as u32;
        let merkle = v
            .get("merkle")
            .and_then(Value::as_array)
            .ok_or_else(|| self.bad("a proof without a branch"))?
            .iter()
            .map(|h| h.as_str().and_then(|s| s.parse().ok()).ok_or_else(|| self.bad("a bad hash in a proof")))
            .collect::<Result<_, _>>()?;
        Ok((merkle, pos))
    }

    pub async fn header(&self, height: u32) -> Result<Header, NetError> {
        let v = self.call("blockchain.block.header", json!([height])).await?;
        let hex = v.as_str().ok_or_else(|| self.bad("a header that is not hex"))?;
        consensus::encode::deserialize_hex(hex).map_err(|e| self.bad(&e.to_string()))
    }

    pub async fn genesis(&self) -> Result<BlockHash, NetError> {
        Ok(self.header(0).await?.block_hash())
    }

    pub async fn tip(&self) -> Result<u32, NetError> {
        let v = self.call("blockchain.headers.subscribe", json!([])).await?;
        v.get("height").and_then(Value::as_u64).map(|h| h as u32).ok_or_else(|| self.bad("no tip height"))
    }

    pub async fn broadcast(&self, tx: &Transaction) -> Result<Txid, NetError> {
        let v = self.call("blockchain.transaction.broadcast", json!([consensus::encode::serialize_hex(tx)])).await?;
        v.as_str().and_then(|s| s.parse().ok()).ok_or_else(|| self.bad("no txid back"))
    }

    pub async fn fee(&self, blocks: u32) -> Result<f64, NetError> {
        let v = self.call("blockchain.estimatefee", json!([blocks])).await?;
        // BTC per kvB, or -1 when it does not know.
        let btc_kvb = v.as_f64().ok_or_else(|| self.bad("no fee"))?;
        if btc_kvb <= 0.0 {
            return Err(self.bad("no estimate"));
        }
        Ok(btc_kvb * 100_000.0)
    }

    fn bad(&self, what: &str) -> NetError {
        NetError::Refused(format!("{}: {what}", self.url))
    }
}

/// Trust a public CA, or the certificate seen the first time, and nothing else.
#[derive(Debug)]
struct Pinning {
    url: String,
    pins: Pins,
    webpki: Arc<rustls::client::WebPkiServerVerifier>,
    refused: Mutex<Option<String>>,
}

impl Pinning {
    fn new(url: String, pins: Pins) -> Pinning {
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        let webpki = rustls::client::WebPkiServerVerifier::builder_with_provider(
            Arc::new(roots),
            Arc::new(rustls::crypto::ring::default_provider()),
        )
        .build()
        .expect("the bundled roots are valid");
        Pinning { url, pins, webpki, refused: Mutex::new(None) }
    }

    fn refused(&self) -> Option<String> {
        self.refused.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl ServerCertVerifier for Pinning {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        inter: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp: String = sha256::Hash::hash(end.as_ref()).to_byte_array().iter().map(|b| format!("{b:02x}")).collect();
        let mut pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
        match pins.get(&self.url) {
            Some(p) if *p == fp => return Ok(ServerCertVerified::assertion()),
            Some(_) if self.webpki.verify_server_cert(end, inter, name, ocsp, now).is_ok() => {
                // A CA-signed certificate that was renewed: fine, follow it.
                pins.insert(self.url.clone(), fp);
                return Ok(ServerCertVerified::assertion());
            }
            Some(_) => {
                *self.refused.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some("its certificate changed since last time, which could be someone in the middle; it is skipped".into());
                return Err(rustls::Error::General("pinned certificate changed".into()));
            }
            None => {}
        }
        pins.insert(self.url.clone(), fp);
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls12_signature(m, c, d)
    }

    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.webpki.verify_tls13_signature(m, c, d)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.webpki.supported_verify_schemes()
    }
}
