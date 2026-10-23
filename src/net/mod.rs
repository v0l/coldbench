//! Talking to the network: scan, fees, broadcast, spread over many public servers.
//!
//! A wallet is a list of addresses, and whoever answers for all of them learns the whole list.
//! So each address is asked of one operator only, chosen by a hash of the address and a secret
//! salt the wallet keeps (rendezvous hashing: an address stays with its operator from sync to
//! sync, and moves to the next one in its own ranking only while that operator is down). A
//! transaction, its proof and its block header are then asked of the server that reported it,
//! so no second server learns of it either.
//!
//! The servers are not trusted: a server answers with a different genesis block is dropped, a
//! transaction must hash to its txid, and a confirmation must come with a merkle proof that leads
//! to a header with real proof of work.
//!
//! What it cannot hide: every server sees your IP address (use Tor or a VPN for that), and
//! spending joins addresses on chain for everyone to see.

pub mod electrum;
pub mod esplora;
pub mod servers;

use bdk_wallet::KeychainKind;
use bdk_wallet::bitcoin::block::Header;
use bdk_wallet::bitcoin::hashes::{Hash, sha256, sha256d};
use bdk_wallet::bitcoin::pow::Target;
use bdk_wallet::bitcoin::{BlockHash, Network, OutPoint, ScriptBuf, Transaction, TxMerkleNode, TxOut, Txid};
use bdk_wallet::chain::spk_client::{FullScanRequest, FullScanResponse};
use bdk_wallet::chain::{BlockId, CheckPoint, ConfirmationBlockTime, TxUpdate};
use futures::future::join_all;
use servers::{Protocol, Server};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;

pub use electrum::Pins;

/// The lowest rate Bitcoin Core relays since 29.1.
pub const MIN_RATE: f64 = 0.1;
/// Requests in flight at once to one server, so a sync does not get us rate limited.
const PER_SERVER: usize = 4;
/// Operators an address is tried on before the scan gives up on it.
const TRIES: usize = 3;

#[derive(Clone, Debug)]
pub enum NetError {
    Down(String),
    Busy(String),
    Refused(String),
    /// The server said something provably false.
    Lied(String),
    Untrusted(String),
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetError::Down(m) => write!(f, "cannot reach {m}"),
            NetError::Busy(m) => write!(f, "rate limited by {m}"),
            NetError::Refused(m) => write!(f, "refused by {m}"),
            NetError::Lied(m) => write!(f, "false answer from {m}"),
            NetError::Untrusted(m) => write!(f, "not trusted: {m}"),
        }
    }
}

/// One line of an address's history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hist {
    pub txid: Txid,
    pub height: Option<u32>,
}

/// What a transaction's inputs spent, where the server says so in passing (Esplora does).
pub type Prevouts = Vec<(OutPoint, TxOut)>;

enum Client {
    Esplora(esplora::Esplora),
    Electrum(electrum::Electrum),
}

pub struct Backend {
    pub server: Server,
    client: Client,
    limit: Semaphore,
}

impl Backend {
    async fn connect(server: Server, pins: Pins) -> Result<Backend, NetError> {
        let client = match server.protocol {
            Protocol::Esplora => Client::Esplora(esplora::Esplora::new(&server.url)),
            Protocol::Electrum => Client::Electrum(electrum::Electrum::connect(&server.url, pins).await?),
        };
        Ok(Backend { server, client, limit: Semaphore::new(PER_SERVER) })
    }

    async fn history(&self, spk: &ScriptBuf) -> Result<(Vec<Hist>, Prevouts), NetError> {
        match &self.client {
            Client::Esplora(c) => c.history(spk).await,
            Client::Electrum(c) => Ok((c.history(spk).await?, Vec::new())),
        }
    }

    async fn tx(&self, txid: Txid) -> Result<Transaction, NetError> {
        match &self.client {
            Client::Esplora(c) => c.tx(txid).await,
            Client::Electrum(c) => c.tx(txid).await,
        }
    }

    async fn proof(&self, txid: Txid, height: u32) -> Result<(Vec<Txid>, u32), NetError> {
        match &self.client {
            Client::Esplora(c) => {
                let (branch, pos, at) = c.proof(txid).await?;
                if at != height {
                    return Err(NetError::Lied(format!("{}: put {txid} at two heights", self.server.url)));
                }
                Ok((branch, pos))
            }
            Client::Electrum(c) => c.proof(txid, height).await,
        }
    }

    async fn header(&self, height: u32) -> Result<Header, NetError> {
        match &self.client {
            Client::Esplora(c) => c.header(height).await,
            Client::Electrum(c) => c.header(height).await,
        }
    }

    async fn tip(&self) -> Result<u32, NetError> {
        match &self.client {
            Client::Esplora(c) => c.tip().await,
            Client::Electrum(c) => c.tip().await,
        }
    }

    async fn genesis(&self) -> Result<BlockHash, NetError> {
        match &self.client {
            Client::Esplora(c) => c.genesis().await,
            Client::Electrum(c) => c.genesis().await,
        }
    }

    async fn broadcast(&self, tx: &Transaction) -> Result<Txid, NetError> {
        match &self.client {
            Client::Esplora(c) => c.broadcast(tx).await,
            Client::Electrum(c) => c.broadcast(tx).await,
        }
    }

    async fn fees(&self) -> Result<Fees, NetError> {
        match &self.client {
            Client::Esplora(c) => Ok(pick(&c.fees().await?)),
            Client::Electrum(c) => {
                let floor = |v: f64| (v.max(MIN_RATE) * 10.0).round() / 10.0;
                Ok(Fees {
                    fast: floor(c.fee(1).await?),
                    medium: floor(c.fee(3).await?),
                    slow: floor(c.fee(6).await?),
                    minimum: floor(c.fee(25).await.unwrap_or(MIN_RATE)),
                })
            }
        }
    }
}

/// What a sync did, for the settings page.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Operator to the addresses it was asked about.
    pub spread: BTreeMap<String, usize>,
    /// Servers that could not be used, and why.
    pub skipped: Vec<String>,
    pub servers: usize,
    pub addresses: usize,
    pub verified: usize,
}

pub struct Pool {
    backends: Vec<Arc<Backend>>,
    /// Operator names, each with its backends' indexes.
    operators: Vec<(String, Vec<usize>)>,
    salt: Vec<u8>,
    limit: Arc<Semaphore>,
    network: Network,
    pub report: Mutex<Report>,
}

impl Pool {
    /// Connect to every server, at most `parallel` at a time, keeping those on `network`.
    pub async fn connect(servers: Vec<Server>, network: Network, salt: &[u8], parallel: usize, pins: Pins) -> Result<Pool, String> {
        let limit = Arc::new(Semaphore::new(parallel.max(1)));
        let genesis = bdk_wallet::bitcoin::constants::genesis_block(network).block_hash();
        let tries = servers.into_iter().map(|s| {
            let (limit, pins) = (limit.clone(), pins.clone());
            async move {
                let _p = limit.acquire().await.expect("never closed");
                let b = Backend::connect(s.clone(), pins).await.map_err(|e| (s.clone(), e.to_string()))?;
                match b.genesis().await {
                    Ok(g) if g == genesis => Ok(b),
                    Ok(_) => Err((s, "it is on another network".to_string())),
                    Err(e) => Err((s, e.to_string())),
                }
            }
        });
        let mut report = Report::default();
        let mut backends = Vec::new();
        for r in join_all(tries).await {
            match r {
                Ok(b) => backends.push(Arc::new(b)),
                Err((s, why)) => report.skipped.push(format!("{}: {why}", s.label())),
            }
        }
        if backends.is_empty() {
            return Err(format!("no server could be used. {}", report.skipped.join("; ")));
        }
        let mut operators: Vec<(String, Vec<usize>)> = Vec::new();
        for (i, b) in backends.iter().enumerate() {
            match operators.iter_mut().find(|(o, _)| *o == b.server.operator) {
                Some((_, v)) => v.push(i),
                None => operators.push((b.server.operator.clone(), vec![i])),
            }
        }
        report.servers = backends.len();
        Ok(Pool { backends, operators, salt: salt.to_vec(), limit, network, report: Mutex::new(report) })
    }

    pub fn operators(&self) -> usize {
        self.operators.len()
    }

    /// The operators to ask about `spk`, best first, the same order every sync.
    fn ranking(&self, spk: &[u8]) -> Vec<usize> {
        let mut scored: Vec<(sha256::Hash, usize)> = self
            .operators
            .iter()
            .enumerate()
            .map(|(i, (name, _))| {
                let mut b = self.salt.clone();
                b.extend_from_slice(name.as_bytes());
                b.push(0);
                b.extend_from_slice(spk);
                (sha256::Hash::hash(&b), i)
            })
            .collect();
        scored.sort_by_key(|s| std::cmp::Reverse(s.0));
        scored.into_iter().map(|(_, i)| i).collect()
    }

    /// Run `f` on a server of each operator in `order` until one answers. `Busy` waits and tries
    /// the same server once more.
    async fn ask<T, F, Fut>(&self, order: &[usize], f: F) -> Result<(Arc<Backend>, T), NetError>
    where
        F: Fn(Arc<Backend>) -> Fut,
        Fut: Future<Output = Result<T, NetError>>,
    {
        let mut last = NetError::Down("no server".into());
        for &op in order.iter().take(TRIES) {
            for &bi in &self.operators[op].1 {
                let b = self.backends[bi].clone();
                for attempt in 0..2 {
                    let r = {
                        let _g = self.limit.acquire().await.expect("never closed");
                        let _s = b.limit.acquire().await.expect("never closed");
                        f(b.clone()).await
                    };
                    match r {
                        Ok(v) => return Ok((b, v)),
                        Err(NetError::Busy(m)) if attempt == 0 => {
                            last = NetError::Busy(m);
                            tokio::time::sleep(Duration::from_millis(2500)).await;
                        }
                        Err(e) => {
                            last = e;
                            break;
                        }
                    }
                }
            }
        }
        Err(last)
    }

    /// Ask `b` only, for what only it should learn about. A busy or flaky server is waited for
    /// rather than swapped, since another server would learn of the transaction.
    async fn on<T, F, Fut>(&self, b: &Backend, f: F) -> Result<T, NetError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T, NetError>>,
    {
        let mut wait = Duration::from_millis(800);
        for attempt in 0..5 {
            let r = {
                let _g = self.limit.acquire().await.expect("never closed");
                let _s = b.limit.acquire().await.expect("never closed");
                f().await
            };
            match r {
                Err(NetError::Busy(_) | NetError::Down(_)) if attempt < 4 => {
                    tokio::time::sleep(wait).await;
                    wait *= 2;
                }
                other => return other,
            }
        }
        unreachable!("the last attempt returns")
    }

    /// Which height most operators say is the tip, and one of them.
    async fn tip(&self) -> Result<(u32, Arc<Backend>), NetError> {
        let asks = self.operators.iter().map(|(_, bs)| {
            let b = self.backends[bs[0]].clone();
            async move { self.on(&b, || b.tip()).await.map(|h| (h, b.clone())) }
        });
        let mut votes: HashMap<u32, Vec<Arc<Backend>>> = HashMap::new();
        for (h, b) in join_all(asks).await.into_iter().flatten() {
            votes.entry(h).or_default().push(b);
        }
        votes
            .into_iter()
            .max_by_key(|(h, v)| (v.len(), *h))
            .map(|(h, v)| (h, v[0].clone()))
            .ok_or_else(|| NetError::Down("every server, asking for the tip".into()))
    }

    /// A header at `height` from `b`, with its work checked.
    async fn header(&self, b: &Backend, height: u32, cache: &Mutex<HashMap<(String, u32), Header>>) -> Result<Header, NetError> {
        let key = (b.server.operator.clone(), height);
        if let Some(h) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return Ok(*h);
        }
        let h = self.on(b, || b.header(height)).await?;
        check_work(&h, self.network).map_err(|why| NetError::Lied(format!("{}: {why}", b.server.url)))?;
        cache.lock().unwrap_or_else(|e| e.into_inner()).insert(key, h);
        Ok(h)
    }

    /// The whole scan. `known` are transactions the wallet already has, `anchored` the ones it
    /// already has as confirmed at that height; neither is fetched again. `floor` is the last
    /// revealed index of each keychain, scanned even if unused.
    pub async fn scan(
        &self,
        mut request: FullScanRequest<KeychainKind>,
        gap: usize,
        known: HashSet<Txid>,
        anchored: HashSet<(Txid, u32)>,
        floor: BTreeMap<KeychainKind, u32>,
        progress: impl Fn(usize),
    ) -> Result<FullScanResponse<KeychainKind>, String> {
        let start = crate::units::now();
        let batch = gap.max(self.limit.available_permits() * 2);
        // txid to the server that reported it, and the height it gave.
        let mut found: HashMap<Txid, (Arc<Backend>, Option<u32>)> = HashMap::new();
        let mut prevouts: BTreeMap<OutPoint, TxOut> = BTreeMap::new();
        let mut last_active = BTreeMap::new();
        let mut scanned = 0usize;
        for keychain in request.keychains() {
            let floor = floor.get(&keychain).copied();
            let mut next = 0u32;
            let mut active: Option<u32> = None;
            loop {
                let spks: Vec<(u32, ScriptBuf)> = request.iter_spks(keychain).take(batch).collect();
                if spks.is_empty() {
                    break;
                }
                let asks = spks.iter().map(|(i, spk)| async move {
                    let order = self.ranking(spk.as_bytes());
                    let s = spk.clone();
                    let r = self.ask(&order, move |b| {
                        let s = s.clone();
                        async move { b.history(&s).await }
                    })
                    .await;
                    (*i, r)
                });
                for (i, r) in join_all(asks).await {
                    let (b, (hist, prev)) = r.map_err(|e| format!("an address could not be checked: {e}"))?;
                    prevouts.extend(prev);
                    *self.report.lock().unwrap_or_else(|e| e.into_inner()).spread.entry(b.server.operator.clone()).or_default() += 1;
                    if !hist.is_empty() {
                        active = Some(active.map_or(i, |a| a.max(i)));
                    }
                    for h in hist {
                        found.entry(h.txid).or_insert((b.clone(), h.height));
                    }
                }
                next += spks.len() as u32;
                scanned += spks.len();
                progress(scanned);
                let past = active.map_or(0, |a| a + 1).max(floor.map_or(0, |f| f + 1));
                if next.saturating_sub(past) as usize >= gap {
                    break;
                }
            }
            if let Some(a) = active {
                last_active.insert(keychain, a);
            }
        }
        self.report.lock().unwrap_or_else(|e| e.into_inner()).addresses = scanned;

        let headers = Mutex::new(HashMap::new());
        let jobs = found.iter().map(|(txid, (b, height))| {
            let (txid, b, height) = (*txid, b.clone(), *height);
            let need_tx = !known.contains(&txid);
            let need_anchor = height.is_some_and(|h| !anchored.contains(&(txid, h)));
            let headers = &headers;
            async move {
                let tx = if need_tx { Some(self.on(&b, || b.tx(txid)).await?) } else { None };
                let anchor = match height {
                    Some(h) if need_anchor => {
                        let (branch, pos) = self.on(&b, || b.proof(txid, h)).await?;
                        let header = self.header(&b, h, headers).await?;
                        if merkle_root(txid, &branch, pos) != header.merkle_root {
                            return Err(NetError::Lied(format!("{}: a proof for {txid} that does not lead to block {h}", b.server.url)));
                        }
                        Some(ConfirmationBlockTime { block_id: BlockId { height: h, hash: header.block_hash() }, confirmation_time: header.time as u64 })
                    }
                    _ => None,
                };
                Ok::<_, NetError>((txid, tx, height, anchor))
            }
        });
        let mut tx_update = TxUpdate::default();
        let mut anchors = Vec::new();
        let mut fetched = Vec::new();
        for r in join_all(jobs).await {
            let (txid, tx, height, anchor) = r.map_err(|e| e.to_string())?;
            if let Some(tx) = tx {
                fetched.push((txid, Arc::new(tx)));
            }
            match (height, anchor) {
                (None, _) => {
                    tx_update.seen_ats.insert((txid, start));
                }
                (Some(_), Some(a)) => {
                    anchors.push(a.block_id);
                    tx_update.anchors.insert((a, txid));
                    self.report.lock().unwrap_or_else(|e| e.into_inner()).verified += 1;
                }
                (Some(_), None) => {}
            }
        }

        // What each new transaction's inputs held, so the wallet can show a payment's flow and
        // fee even when the coins were someone else's. Esplora says so with the history;
        // otherwise ask the server that reported the transaction for the parents, which it knows
        // of anyway. These only feed the picture and the fee shown for other people's coins:
        // what we own is always worked out from transactions checked against their txids.
        let missing: Vec<(Arc<Backend>, Txid)> = {
            let mut want = BTreeMap::new();
            // Only for payments that spend one of ours (an input's parent is also one of the new or
            // known transactions): those are the ones whose fee the wallet shows. Fetching the
            // parents of every receive would cost a whole transaction per stranger's coin.
            let ours: HashSet<Txid> = found.keys().copied().chain(known.iter().copied()).collect();
            for (txid, tx) in &fetched {
                let spends_ours = tx.input.iter().any(|i| ours.contains(&i.previous_output.txid));
                if !spends_ours {
                    continue;
                }
                for i in &tx.input {
                    if !prevouts.contains_key(&i.previous_output) && !i.previous_output.is_null() {
                        want.entry(i.previous_output.txid).or_insert_with(|| found[txid].0.clone());
                    }
                }
            }
            want.into_iter().map(|(t, b)| (b, t)).collect()
        };
        let parents = missing.iter().map(|(b, t)| async move { self.on(b, || b.tx(*t)).await.ok() });
        for parent in join_all(parents).await.into_iter().flatten() {
            let txid = parent.compute_txid();
            for (n, o) in parent.output.into_iter().enumerate() {
                prevouts.insert(OutPoint::new(txid, n as u32), o);
            }
        }
        for (_, tx) in &fetched {
            for i in &tx.input {
                if let Some(o) = prevouts.get(&i.previous_output) {
                    tx_update.txouts.insert(i.previous_output, o.clone());
                }
            }
        }
        tx_update.txs.extend(fetched.into_iter().map(|(_, t)| t));

        let chain_update = match request.chain_tip() {
            Some(local) => Some(self.chain(local, anchors).await.map_err(|e| e.to_string())?),
            None => None,
        };
        Ok(FullScanResponse { tx_update, last_active_indices: last_active, chain_update })
    }

    /// A chain update that connects to `local`: the point where the servers agree with it, the
    /// blocks the new confirmations are in, and the tip.
    async fn chain(&self, local: CheckPoint, anchors: Vec<BlockId>) -> Result<CheckPoint, NetError> {
        let (tip_h, b) = self.tip().await?;
        let tip_header = self.header(&b, tip_h, &Mutex::default()).await?;
        let mut agree = None;
        let mut conflicts = Vec::new();
        for cp in local.iter() {
            if cp.height() > tip_h {
                continue;
            }
            let hash = if cp.height() == 0 {
                cp.hash()
            } else {
                self.header(&b, cp.height(), &Mutex::default()).await?.block_hash()
            };
            if hash == cp.hash() {
                agree = Some(cp);
                break;
            }
            conflicts.push(BlockId { height: cp.height(), hash });
        }
        let mut tip = agree.ok_or_else(|| NetError::Lied(format!("{}: shares no block with the saved chain", b.server.url)))?;
        tip = tip.extend(conflicts.into_iter().rev()).expect("conflicts are in order");
        for a in anchors {
            if tip.get(a.height).is_none_or(|c| c.hash() != a.hash) {
                tip = tip.insert(a);
            }
        }
        Ok(tip.insert(BlockId { height: tip_h, hash: tip_header.block_hash() }))
    }

    /// Hand `tx` to the operator that already answers for `hint` (its first input's address), so
    /// no extra server links you to it.
    pub async fn broadcast(&self, tx: &Transaction, hint: &ScriptBuf) -> Result<Txid, String> {
        let order = self.ranking(hint.as_bytes());
        let tx = tx.clone();
        let (_, txid) = self
            .ask(&order, move |b| {
                let tx = tx.clone();
                async move { b.broadcast(&tx).await }
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(txid)
    }

    /// Estimates from the first operator that has them. Asking reveals nothing about the wallet.
    pub async fn fees(&self) -> Result<Fees, String> {
        let order: Vec<usize> = (0..self.operators.len()).collect();
        let mut last = String::new();
        for chunk in order.chunks(TRIES) {
            match self.ask(chunk, |b| async move { b.fees().await }).await {
                Ok((_, f)) => return Ok(f),
                Err(e) => last = e.to_string(),
            }
        }
        Err(last)
    }
}

/// The price of a bitcoin from whichever Esplora server answers first. It needs no pool: no
/// handshakes with servers that are never asked, and nothing about the wallet is sent, so a few
/// servers are raced at once and the rest are only tried if all of those fail.
pub async fn prices(servers: &[Server]) -> Result<Prices, String> {
    let esplora: Vec<&Server> = servers.iter().filter(|s| s.protocol == Protocol::Esplora).collect();
    if esplora.is_empty() {
        return Err("none of the servers is an Esplora server, which is what gives prices".into());
    }
    let mut last = String::from("no server");
    for group in esplora.chunks(3) {
        let asks = group.iter().map(|s| {
            let c = esplora::Esplora::with_timeout(&s.url, Duration::from_secs(6));
            Box::pin(async move { c.prices().await.map_err(|e| e.to_string()) })
        });
        match futures::future::select_ok(asks).await {
            Ok((rates, _)) if !rates.is_empty() => return Ok(Prices { rates }),
            Ok(_) => last = "the server had no prices".into(),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// What one bitcoin costs, by ISO 4217 code.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prices {
    pub rates: BTreeMap<String, f64>,
}

impl Prices {
    /// The last prices, kept between runs so there is one to show before the first answer.
    pub fn load(file: &std::path::Path) -> Option<Prices> {
        let rates = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
        Some(Prices { rates })
    }

    pub fn save(&self, file: &std::path::Path) {
        if let Ok(t) = serde_json::to_string(&self.rates) {
            let _ = std::fs::write(file, t);
        }
    }
}

/// Hash `txid` up its branch to the merkle root.
pub fn merkle_root(txid: Txid, branch: &[Txid], pos: u32) -> TxMerkleNode {
    let mut h = txid.to_byte_array();
    let mut pos = pos;
    for sib in branch {
        let s = sib.to_byte_array();
        let mut buf = [0u8; 64];
        if pos & 1 == 0 {
            buf[..32].copy_from_slice(&h);
            buf[32..].copy_from_slice(&s);
        } else {
            buf[..32].copy_from_slice(&s);
            buf[32..].copy_from_slice(&h);
        }
        h = sha256d::Hash::hash(&buf).to_byte_array();
        pos >>= 1;
    }
    TxMerkleNode::from_byte_array(h)
}

/// The header carries the work it claims, and on mainnet that is more work than anyone could
/// spend on a fake (the difficulty of 2017 or more).
fn check_work(h: &Header, network: Network) -> Result<(), String> {
    h.validate_pow(h.target()).map_err(|_| "a header without the work it claims".to_string())?;
    if network == Network::Bitcoin {
        let floor = Target::from_compact(bdk_wallet::bitcoin::CompactTarget::from_consensus(0x1800_ffff));
        if h.target() > floor {
            return Err("a header far too easy for mainnet".into());
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fees {
    /// Sat/vB for the next block, within three, within six and within a day.
    pub fast: f64,
    pub medium: f64,
    pub slow: f64,
    pub minimum: f64,
}

fn pick(map: &HashMap<u16, f64>) -> Fees {
    // Esplora answers for targets in blocks. Use the nearest target at or above the one asked for.
    let at = |target: u16| -> f64 {
        let mut keys: Vec<&u16> = map.keys().collect();
        keys.sort();
        keys.into_iter().find(|k| **k >= target).or_else(|| map.keys().max()).and_then(|k| map.get(k)).copied().unwrap_or(1.0)
    };
    // Blocks clear below 1 sat/vB now; nodes relay down to 0.1.
    let floor = |v: f64| (v.max(MIN_RATE) * 10.0).round() / 10.0;
    Fees { fast: floor(at(1)), medium: floor(at(3)), slow: floor(at(6)), minimum: floor(at(1008)) }
}

#[cfg(test)]
mod tests;
