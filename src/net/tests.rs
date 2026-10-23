use super::*;
use bdk_wallet::bitcoin::consensus::encode::deserialize_hex;

/// Block 170, the first one with a payment between people (Satoshi to Hal Finney).
const H170: &str = "0100000055bd840a78798ad0da853f68974f3d183e2bd1db6a842c1feecf222a00000000ff104ccb05421ab93e63f8c3ce5c2c2e9dbb37de2764b3a3175c8166562cac7d51b96a49ffff001d283e9e70";

fn pool_of(ops: &[&str]) -> Pool {
    Pool {
        backends: Vec::new(),
        operators: ops.iter().map(|o| (o.to_string(), Vec::new())).collect(),
        salt: b"salt".to_vec(),
        limit: Arc::new(Semaphore::new(8)),
        network: Network::Bitcoin,
        report: Mutex::default(),
    }
}

#[test]
fn a_real_merkle_proof_leads_to_its_block() {
    let header: Header = deserialize_hex(H170).unwrap();
    let txid: Txid = "f4184fc596403b9d638783cf57adfe4c75c605f6356fbc91338530e9831e9e16".parse().unwrap();
    let sib: Txid = "b1fea52486ce0c62bb442b530a3f0132b826c74e473d1f2c220bfa78111c5082".parse().unwrap();
    assert_eq!(merkle_root(txid, &[sib], 1), header.merkle_root);
    assert_ne!(merkle_root(txid, &[sib], 0), header.merkle_root, "the wrong side does not");
    assert!(check_work(&header, Network::Testnet).is_ok());
}

#[test]
fn a_header_without_work_is_refused() {
    let mut header: Header = deserialize_hex(H170).unwrap();
    header.nonce ^= 1;
    assert!(check_work(&header, Network::Testnet).is_err());
    // Block 170 is real but far too easy to trust as a recent mainnet block.
    let header: Header = deserialize_hex(H170).unwrap();
    assert!(check_work(&header, Network::Bitcoin).is_err());
}

#[test]
fn an_address_keeps_its_operator_and_they_share_the_load() {
    let pool = pool_of(&["a", "b", "c", "d", "e"]);
    let mut count = [0usize; 5];
    for i in 0..1000u32 {
        let spk = i.to_le_bytes();
        let r = pool.ranking(&spk);
        assert_eq!(r, pool.ranking(&spk), "the same every time");
        let mut sorted = r.clone();
        sorted.sort();
        assert_eq!(sorted, vec![0, 1, 2, 3, 4], "every operator ranked once");
        count[r[0]] += 1;
    }
    assert!(count.iter().all(|&c| (130..=270).contains(&c)), "{count:?}");
}

#[test]
fn another_salt_gives_another_split() {
    let a = pool_of(&["a", "b", "c", "d"]);
    let mut b = pool_of(&["a", "b", "c", "d"]);
    b.salt = b"other".to_vec();
    let differ = (0..200u32).filter(|i| a.ranking(&i.to_le_bytes())[0] != b.ranking(&i.to_le_bytes())[0]).count();
    assert!(differ > 100, "{differ}");
}

#[test]
fn losing_an_operator_moves_only_its_addresses() {
    let full = pool_of(&["a", "b", "c", "d"]);
    let less = pool_of(&["a", "b", "c"]);
    for i in 0..500u32 {
        let s = i.to_le_bytes();
        let first = full.ranking(&s)[0];
        if first != 3 {
            assert_eq!(less.ranking(&s)[0], first, "addresses not on d stay put");
        }
    }
}

#[test]
fn fee_targets_pick_the_nearest_estimate() {
    let map = [(1u16, 40.0), (2, 30.0), (4, 20.0), (6, 12.0), (1008, 1.0)].into_iter().collect();
    let f = pick(&map);
    assert_eq!((f.fast, f.medium, f.slow, f.minimum), (40.0, 20.0, 12.0, 1.0));
}

#[test]
fn an_empty_answer_still_gives_a_usable_rate() {
    let f = pick(&Default::default());
    assert_eq!(f.fast, 1.0, "no estimates falls back to 1 sat/vB");
}

mod live {
    use super::*;

    /// Scans the public BIP84 test wallet across every public server and checks it against one
    /// server's own scan through bdk_esplora.
    #[test]
    #[ignore = "needs the network"]
    fn the_split_scan_matches_a_single_server_scan() {
        use bdk_esplora::{EsploraExt, esplora_client};
        let xpub = "xpub6CatWdiZiodmUeTDp8LT5or8nmbKNcuyvz7WyksVFkKB4RHwCD3XyuvPEbvqAQY3rAPshWcMLoP2fMFMKHPJ4ZeZXYVUhLv1VMrjPC7PW6V";
        let fp = "73c5da0a";
        let desc = |c: u32| format!("wpkh([{fp}/84'/0'/0']{xpub}/{c}/*)");
        let make = || bdk_wallet::Wallet::create(desc(0), desc(1)).network(Network::Bitcoin).create_wallet_no_persist().unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let pool = rt
            .block_on(Pool::connect(servers::public(Network::Bitcoin), Network::Bitcoin, b"test", 12, Pins::default()))
            .unwrap();
        println!("{} servers, {} operators", pool.backends.len(), pool.operators());
        let mut split = make();
        let req = split.start_full_scan().build();
        let r = rt.block_on(pool.scan(req, 20, HashSet::new(), HashSet::new(), BTreeMap::new(), |_| {})).unwrap();
        split.apply_update(r).unwrap();
        println!("{:#?}", pool.report.lock().unwrap());

        let mut one = make();
        let c = esplora_client::Builder::new("https://mempool.space/api").build_blocking();
        let r = c.full_scan(one.start_full_scan().build(), 20, 4).unwrap();
        one.apply_update(r).unwrap();

        let txids = |w: &bdk_wallet::Wallet| {
            let mut v: Vec<Txid> = w.transactions().map(|t| t.tx_node.txid).collect();
            v.sort();
            v
        };
        let (a, b) = (txids(&split), txids(&one));
        let only_split: Vec<_> = a.iter().filter(|t| !b.contains(t)).collect();
        let only_one: Vec<_> = b.iter().filter(|t| !a.contains(t)).collect();
        println!("split {} one {}; only split {:?}; only one {:?}", a.len(), b.len(), only_split, only_one);
        let idx = |w: &bdk_wallet::Wallet| (w.spk_index().last_revealed_index(bdk_wallet::KeychainKind::External), w.spk_index().last_revealed_index(bdk_wallet::KeychainKind::Internal));
        println!("revealed split {:?} one {:?}", idx(&split), idx(&one));
        // The split scan works in batches of 20 and finishes the batch it is in, so it can look
        // a little past a gap of 20 and find more; it must never find less.
        assert!(only_one.is_empty(), "missed {only_one:?}");
        assert!(split.balance().total() >= one.balance().total());
        let r = pool.report.lock().unwrap();
        assert!(r.spread.len() >= 3, "spread over {:?}", r.spread);
    }
}

#[test]
#[ignore = "needs the network"]
fn prices_come_from_a_public_server() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let t = std::time::Instant::now();
    let p = rt.block_on(prices(&servers::public(Network::Bitcoin))).unwrap();
    println!("{:?} in {:?}", p.rates.get("USD"), t.elapsed());
    assert!(p.rates["USD"] > 1000.0);
}
