use crate::engine::analysis::{Finding, Severity, is_coinjoin, payout_shape, peel_chain_length};
use bdk_wallet::bitcoin::{OutPoint, ScriptBuf, Transaction, TxOut, Txid};
use std::collections::{HashMap, HashSet, VecDeque};

pub const MAX_HOPS: u32 = 6;
const MAX_TXS: usize = 150;
const PAYOUT_OUTPUTS: usize = 10;
const PEEL_MIN: usize = 3;

pub trait Lookup {
    fn tx(&self, txid: Txid) -> Result<Transaction, String>;
    fn spender(&self, outpoint: OutPoint, spk: &ScriptBuf) -> Result<Option<Txid>, String>;
}

struct Seen {
    tx: Transaction,
    prevouts: Vec<TxOut>,
    depth: u32,
}

struct Walk<'a> {
    lookup: &'a dyn Lookup,
    cache: HashMap<Txid, Transaction>,
}

impl Walk<'_> {
    fn tx(&mut self, txid: Txid) -> Result<Transaction, String> {
        if let Some(t) = self.cache.get(&txid) {
            return Ok(t.clone());
        }
        let t = self.lookup.tx(txid)?;
        self.cache.insert(txid, t.clone());
        Ok(t)
    }

    fn prevouts(&mut self, tx: &Transaction) -> Result<Vec<TxOut>, String> {
        if tx.is_coinbase() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for i in &tx.input {
            let parent = self.tx(i.previous_output.txid)?;
            let o = parent
                .output
                .get(i.previous_output.vout as usize)
                .ok_or("the server's parent transaction has no such output")?;
            out.push(o.clone());
        }
        Ok(out)
    }

    fn forward(&mut self, tx: &Transaction, index: usize) -> Result<Option<Transaction>, String> {
        let spk = &tx.output[index].script_pubkey;
        if spk.is_op_return() {
            return Ok(None);
        }
        let outpoint = OutPoint::new(tx.compute_txid(), index as u32);
        match self.lookup.spender(outpoint, spk)? {
            Some(t) => self.tx(t).map(Some),
            None => Ok(None),
        }
    }
}

pub fn trace(lookup: &dyn Lookup, start: &[Txid], hops: u32) -> Result<Vec<Finding>, String> {
    let hops = hops.min(MAX_HOPS);
    let mut walk = Walk { lookup, cache: HashMap::new() };
    let mut seen: HashMap<Txid, Seen> = HashMap::new();
    let mut queue: VecDeque<(Txid, u32)> = VecDeque::new();
    for &t in start {
        let tx = walk.tx(t)?;
        let prevouts = walk.prevouts(&tx)?;
        seen.insert(t, Seen { tx, prevouts, depth: 0 });
        queue.push_back((t, 0));
    }
    while let Some((txid, depth)) = queue.pop_front() {
        if depth >= hops || seen.len() >= MAX_TXS {
            continue;
        }
        let tx = seen[&txid].tx.clone();
        let mut next: Vec<Transaction> = Vec::new();
        for i in 0..tx.output.len() {
            if let Some(t) = walk.forward(&tx, i)? {
                next.push(t);
            }
        }
        if !tx.is_coinbase() {
            for i in &tx.input {
                next.push(walk.tx(i.previous_output.txid)?);
            }
        }
        for t in next {
            let id = t.compute_txid();
            if seen.contains_key(&id) || seen.len() >= MAX_TXS {
                continue;
            }
            let prevouts = walk.prevouts(&t)?;
            seen.insert(id, Seen { tx: t, prevouts, depth: depth + 1 });
            queue.push_back((id, depth + 1));
        }
    }

    let mut out = Vec::new();
    let mut ids: Vec<&Txid> = seen.keys().collect();
    ids.sort_by_key(|t| (seen[*t].depth, **t));
    for id in ids {
        let s = &seen[id];
        if s.depth == 0 {
            continue;
        }
        let near = if s.depth == 1 { "1 hop".to_string() } else { format!("{} hops", s.depth) };
        if let Some(n) = payout_shape(&s.tx, PAYOUT_OUTPUTS) {
            out.push(Finding {
                severity: Severity::Notice,
                text: format!("{} is {near} from your coins and pays {n} outputs of mixed types, like an exchange or service", short(id)),
                fix: Some("coins that touched a service are probably known to it".to_string()),
            });
        }
        if is_coinjoin(&s.tx, &s.prevouts) {
            out.push(Finding {
                severity: Severity::Good,
                text: format!("{} is {near} from your coins and looks like a coinjoin", short(id)),
                fix: None,
            });
        }
    }

    let mut peeled: HashSet<Txid> = HashSet::new();
    for &t in start {
        let mut chain: Vec<(Transaction, Vec<TxOut>)> = Vec::new();
        let mut cur = seen[&t].tx.clone();
        let mut guard = 0;
        loop {
            guard += 1;
            if cur.output.len() != 2 || guard > hops as usize + 1 {
                break;
            }
            let prevouts = walk.prevouts(&cur)?;
            let big = (0..2).max_by_key(|&i| cur.output[i].value).expect("two outputs");
            let next = walk.forward(&cur, big)?;
            chain.push((cur.clone(), prevouts));
            match next {
                Some(n) => cur = n,
                None => break,
            }
        }
        let run = peel_chain_length(&chain);
        if run >= PEEL_MIN && peeled.insert(t) {
            out.push(Finding {
                severity: Severity::Warning,
                text: format!("{} follows a peel chain of {run} payments, so the big output is followed hop by hop", short(&t)),
                fix: Some("do not spend the remainder straight after a payment, and mix its source".to_string()),
            });
        }
    }
    Ok(out)
}

fn short(t: &Txid) -> String {
    let s = t.to_string();
    format!("{}...", &s[..8])
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdk_wallet::bitcoin::absolute::LockTime;
    use bdk_wallet::bitcoin::transaction::Version;
    use bdk_wallet::bitcoin::{Amount, Sequence, TxIn, Witness};
    use std::cell::Cell;

    struct Fake {
        txs: HashMap<Txid, Transaction>,
        asked: Cell<usize>,
    }

    impl Lookup for Fake {
        fn tx(&self, txid: Txid) -> Result<Transaction, String> {
            self.asked.set(self.asked.get() + 1);
            self.txs.get(&txid).cloned().ok_or_else(|| "unknown tx".to_string())
        }

        fn spender(&self, outpoint: OutPoint, _spk: &ScriptBuf) -> Result<Option<Txid>, String> {
            self.asked.set(self.asked.get() + 1);
            Ok(self.txs.values().find(|t| t.input.iter().any(|i| i.previous_output == outpoint)).map(|t| t.compute_txid()))
        }
    }

    fn wpkh(n: u8) -> ScriptBuf {
        let mut b = vec![0x00, 0x14];
        b.extend([n; 20]);
        ScriptBuf::from_bytes(b)
    }

    fn make(inputs: Vec<OutPoint>, outputs: Vec<(ScriptBuf, u64)>) -> Transaction {
        Transaction {
            version: Version(2),
            lock_time: LockTime::ZERO,
            input: inputs
                .into_iter()
                .map(|p| TxIn { previous_output: p, script_sig: ScriptBuf::new(), sequence: Sequence(0xffff_fffd), witness: Witness::new() })
                .collect(),
            output: outputs.into_iter().map(|(s, v)| TxOut { value: Amount::from_sat(v), script_pubkey: s }).collect(),
        }
    }

    fn root() -> Transaction {
        make(vec![OutPoint::null()], vec![(wpkh(1), 10_000_000)])
    }

    fn fake(list: Vec<Transaction>) -> Fake {
        Fake { txs: list.into_iter().map(|t| (t.compute_txid(), t)).collect(), asked: Cell::new(0) }
    }

    #[test]
    fn a_peel_chain_after_the_users_payment_is_found() {
        let funding = root();
        let mut list = vec![funding.clone()];
        let mut prev = OutPoint::new(funding.compute_txid(), 0);
        let mut value = 10_000_000u64;
        let mut first = None;
        for step in 0..5u8 {
            let tx = make(vec![prev], vec![(wpkh(50 + step), 100_000 + step as u64), (wpkh(100 + step), value - 100_500)]);
            first.get_or_insert(tx.compute_txid());
            prev = OutPoint::new(tx.compute_txid(), 1);
            value -= 100_500;
            list.push(tx);
        }
        let f = fake(list);
        let found = trace(&f, &[first.unwrap()], 5).unwrap();
        assert!(found.iter().any(|x| x.text.contains("peel chain")), "{found:?}");
    }

    #[test]
    fn an_exchange_like_payout_nearby_is_named() {
        let funding = root();
        let spend = make(vec![OutPoint::new(funding.compute_txid(), 0)], vec![(wpkh(9), 9_000_000)]);
        let outs: Vec<(ScriptBuf, u64)> = (0..12u8)
            .map(|n| {
                let mut b = vec![0xa9, 0x14];
                b.extend([n; 20]);
                b.push(0x87);
                (if n % 2 == 0 { wpkh(n) } else { ScriptBuf::from_bytes(b) }, 10_000 + n as u64)
            })
            .collect();
        let payout = make(vec![OutPoint::new(spend.compute_txid(), 0)], outs);
        let f = fake(vec![funding, spend.clone(), payout]);
        let found = trace(&f, &[spend.compute_txid()], 2).unwrap();
        assert!(found.iter().any(|x| x.text.contains("1 hop") && x.text.contains("exchange")), "{found:?}");
    }

    #[test]
    fn the_hop_limit_stops_the_walk() {
        let funding = root();
        let mut list = vec![funding.clone()];
        let mut prev = OutPoint::new(funding.compute_txid(), 0);
        let mut first = None;
        for step in 0..6u8 {
            let tx = make(vec![prev], vec![(wpkh(10 + step), 9_000_000 - step as u64)]);
            first.get_or_insert(tx.compute_txid());
            prev = OutPoint::new(tx.compute_txid(), 0);
            list.push(tx);
        }
        let near = fake(list.clone());
        trace(&near, &[first.unwrap()], 1).unwrap();
        let far = fake(list);
        trace(&far, &[first.unwrap()], 6).unwrap();
        assert!(near.asked.get() < far.asked.get());
    }

    #[test]
    fn a_server_error_is_reported_not_swallowed() {
        let f = fake(vec![]);
        assert!(trace(&f, &[Txid::from_raw_hash(bdk_wallet::bitcoin::hashes::Hash::hash(&[1]))], 2).is_err());
    }

    #[test]
    fn only_a_listed_esplora_server_is_ever_chosen() {
        use super::super::servers::Server;
        assert!(own_esplora(&[]).is_none());
        assert!(own_esplora(&[Server::electrum("host:50002", "x")]).is_none());
        let both = [Server::electrum("host:50002", "x"), Server::esplora("http://127.0.0.1:1/api", "y")];
        assert_eq!(own_esplora(&both).unwrap().url, "http://127.0.0.1:1/api");
    }

    #[test]
    fn a_trace_over_http_asks_only_the_fake_server_and_finds_the_peel_chain() {
        use bdk_wallet::bitcoin::consensus::encode::serialize_hex;
        use std::io::{Read, Write};
        let funding = root();
        let mut list = vec![funding.clone()];
        let mut prev = OutPoint::new(funding.compute_txid(), 0);
        let mut value = 10_000_000u64;
        let mut first = None;
        for step in 0..5u8 {
            let tx = make(vec![prev], vec![(wpkh(50 + step), 100_000 + step as u64), (wpkh(100 + step), value - 100_500)]);
            first.get_or_insert(tx.compute_txid());
            prev = OutPoint::new(tx.compute_txid(), 1);
            value -= 100_500;
            list.push(tx);
        }
        let by_id: HashMap<Txid, Transaction> = list.iter().map(|t| (t.compute_txid(), t.clone())).collect();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let paths = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let log = paths.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(path.clone());
                let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
                let body = match parts.as_slice() {
                    ["api", "tx", id, "hex"] => by_id.get(&id.parse::<Txid>().unwrap()).map(serialize_hex),
                    ["api", "tx", id, "outspend", vout] => {
                        let op = OutPoint::new(id.parse().unwrap(), vout.parse().unwrap());
                        let hit = by_id.values().find(|t| t.input.iter().any(|i| i.previous_output == op));
                        Some(match hit {
                            Some(t) => format!("{{\"spent\":true,\"txid\":\"{}\"}}", t.compute_txid()),
                            None => "{\"spent\":false}".to_string(),
                        })
                    }
                    _ => None,
                };
                let reply = match body {
                    Some(b) => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}", b.len()),
                    None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                };
                let _ = s.write_all(reply.as_bytes());
            }
        });
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let remote = Remote::new(&format!("http://127.0.0.1:{port}/api"), rt.handle().clone());
        let found = std::thread::spawn(move || trace(&remote, &[first.unwrap()], 5)).join().unwrap().unwrap();
        assert!(found.iter().any(|x| x.text.contains("peel chain")), "{found:?}");
        let asked = paths.lock().unwrap();
        assert!(!asked.is_empty() && asked.iter().all(|p| p.starts_with("/api/tx/")), "{asked:?}");
    }
}

pub struct Remote {
    esplora: super::esplora::Esplora,
    rt: tokio::runtime::Handle,
}

impl Remote {
    pub fn new(url: &str, rt: tokio::runtime::Handle) -> Remote {
        Remote { esplora: super::esplora::Esplora::new(url), rt }
    }
}

impl Lookup for Remote {
    fn tx(&self, txid: Txid) -> Result<Transaction, String> {
        self.rt.block_on(self.esplora.tx(txid)).map_err(|e| e.to_string())
    }

    fn spender(&self, outpoint: OutPoint, _spk: &ScriptBuf) -> Result<Option<Txid>, String> {
        self.rt.block_on(self.esplora.spender(outpoint.txid, outpoint.vout)).map_err(|e| e.to_string())
    }
}

pub fn own_esplora(servers: &[super::servers::Server]) -> Option<&super::servers::Server> {
    servers.iter().find(|s| s.protocol == super::servers::Protocol::Esplora)
}
