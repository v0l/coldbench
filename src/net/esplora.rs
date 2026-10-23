//! Esplora over HTTPS. Asked directly with reqwest rather than through `esplora_client`'s async
//! client, which treats mempool.space's 203 answers as errors; its JSON types are reused.

use super::{Hist, NetError};
use bdk_wallet::bitcoin::block::Header;
use bdk_wallet::bitcoin::hashes::{Hash, sha256};
use bdk_wallet::bitcoin::{Amount, BlockHash, OutPoint, Script, Transaction, TxOut, Txid, consensus};
use esplora_client::api::{MerkleProof, Tx};
use std::time::Duration;

pub struct Esplora {
    url: String,
    http: reqwest::Client,
}

impl Esplora {
    pub fn new(url: &str) -> Esplora {
        Esplora::with_timeout(url, Duration::from_secs(25))
    }

    pub fn with_timeout(url: &str, timeout: Duration) -> Esplora {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent("coldbench")
            .build()
            .expect("a client with no custom settings builds");
        Esplora { url: url.trim_end_matches('/').to_string(), http }
    }

    async fn get(&self, path: &str) -> Result<reqwest::Response, NetError> {
        let r = self.http.get(format!("{}{path}", self.url)).send().await.map_err(|e| self.down(e))?;
        let code = r.status().as_u16();
        match code {
            200..=299 => Ok(r),
            429 => Err(NetError::Busy(format!("{}: too many requests", self.url))),
            404 => Err(NetError::Refused(format!("{}: not found", self.url))),
            _ => Err(NetError::Down(format!("{}: answered {code}", self.url))),
        }
    }

    async fn text(&self, path: &str) -> Result<String, NetError> {
        self.get(path).await?.text().await.map_err(|e| self.down(e))
    }

    async fn json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, NetError> {
        let body = self.get(path).await?.bytes().await.map_err(|e| self.down(e))?;
        serde_json::from_slice(&body).map_err(|e| NetError::Refused(format!("{}: {e}", self.url)))
    }

    fn down(&self, e: reqwest::Error) -> NetError {
        let why = if e.is_timeout() {
            "did not answer in time".to_string()
        } else if e.is_connect() {
            "cannot connect".to_string()
        } else {
            e.to_string()
        };
        NetError::Down(format!("{}: {why}", self.url))
    }

    /// Every transaction touching `spk`. Esplora pages confirmed history 25 at a time.
    /// Also returns what each transaction's inputs spent, which Esplora includes.
    pub async fn history(&self, spk: &Script) -> Result<(Vec<Hist>, super::Prevouts), NetError> {
        let hash = sha256::Hash::hash(spk.as_bytes());
        let mut out = Vec::new();
        let mut prev = Vec::new();
        let mut last: Option<Txid> = None;
        loop {
            let path = match last {
                None => format!("/scripthash/{hash:x}/txs"),
                Some(t) => format!("/scripthash/{hash:x}/txs/chain/{t}"),
            };
            let page: Vec<Tx> = self.json(&path).await?;
            let confirmed = page.iter().filter(|t| t.status.confirmed).count();
            let n = page.len();
            last = page.iter().rev().find(|t| t.status.confirmed).map(|t| t.txid);
            for t in &page {
                for v in &t.vin {
                    if let Some(p) = &v.prevout {
                        let o = TxOut { value: Amount::from_sat(p.value), script_pubkey: p.scriptpubkey.clone() };
                        prev.push((OutPoint::new(v.txid, v.vout), o));
                    }
                }
            }
            out.extend(page.into_iter().map(|t| Hist { txid: t.txid, height: t.status.block_height.filter(|_| t.status.confirmed) }));
            // The first page holds up to 50 mempool and 25 confirmed; later pages 25 confirmed.
            if confirmed < 25 || n == 0 || last.is_none() {
                break;
            }
        }
        out.sort_by_key(|h| h.txid);
        out.dedup_by_key(|h| h.txid);
        Ok((out, prev))
    }

    pub async fn tx(&self, txid: Txid) -> Result<Transaction, NetError> {
        let hex = self.text(&format!("/tx/{txid}/hex")).await?;
        let tx: Transaction = consensus::encode::deserialize_hex(hex.trim()).map_err(|e| NetError::Refused(format!("{}: {e}", self.url)))?;
        if tx.compute_txid() != txid {
            return Err(NetError::Lied(format!("{}: sent a different transaction than asked for", self.url)));
        }
        Ok(tx)
    }

    pub async fn proof(&self, txid: Txid) -> Result<(Vec<Txid>, u32, u32), NetError> {
        let p: MerkleProof = self.json(&format!("/tx/{txid}/merkle-proof")).await?;
        Ok((p.merkle, p.pos as u32, p.block_height))
    }

    pub async fn header(&self, height: u32) -> Result<Header, NetError> {
        let hash: BlockHash = self.text(&format!("/block-height/{height}")).await?.trim().parse().map_err(|_| NetError::Refused(format!("{}: a bad block hash", self.url)))?;
        let hex = self.text(&format!("/block/{hash}/header")).await?;
        let h: Header = consensus::encode::deserialize_hex(hex.trim()).map_err(|e| NetError::Refused(format!("{}: {e}", self.url)))?;
        if h.block_hash() != hash {
            return Err(NetError::Lied(format!("{}: a header that does not hash to its block", self.url)));
        }
        Ok(h)
    }

    pub async fn genesis(&self) -> Result<BlockHash, NetError> {
        self.text("/block-height/0").await?.trim().parse().map_err(|_| NetError::Refused(format!("{}: a bad block hash", self.url)))
    }

    pub async fn tip(&self) -> Result<u32, NetError> {
        self.text("/blocks/tip/height").await?.trim().parse().map_err(|_| NetError::Refused(format!("{}: a bad height", self.url)))
    }

    pub async fn broadcast(&self, tx: &Transaction) -> Result<Txid, NetError> {
        let r = self
            .http
            .post(format!("{}/tx", self.url))
            .body(consensus::encode::serialize_hex(tx))
            .send()
            .await
            .map_err(|e| self.down(e))?;
        let ok = r.status().is_success();
        let body = r.text().await.unwrap_or_default();
        if !ok {
            return Err(NetError::Refused(format!("{}: {}", self.url, body.trim())));
        }
        body.trim().parse().map_err(|_| NetError::Refused(format!("{}: {}", self.url, body.trim())))
    }

    pub async fn fees(&self) -> Result<std::collections::HashMap<u16, f64>, NetError> {
        let raw: std::collections::HashMap<String, f64> = self.json("/fee-estimates").await?;
        Ok(raw.into_iter().filter_map(|(k, v)| Some((k.parse().ok()?, v))).collect())
    }

    /// Bitcoin's price in each fiat currency, from mempool.space's API (`/v1/prices`).
    pub async fn prices(&self) -> Result<std::collections::BTreeMap<String, f64>, NetError> {
        let raw: std::collections::BTreeMap<String, f64> = self.json("/v1/prices").await?;
        Ok(raw.into_iter().filter(|(k, v)| k.len() == 3 && k.chars().all(|c| c.is_ascii_uppercase()) && *v > 0.0).collect())
    }
}
