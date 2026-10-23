//! The send, confirm and fee bump flow against a real regtest node. Needs the containers from
//! `scripts/regtest.sh up` (bitcoind with a `miner` wallet, and Esplora on 127.0.0.1:3002), so it
//! is ignored by default: `cargo test --test regtest -- --ignored --nocapture`.
use bdk_wallet::bitcoin::bip32::{Xpriv, Xpub};
use bdk_wallet::bitcoin::secp256k1::Secp256k1;
use bdk_wallet::bitcoin::{Address, Network, Psbt, Txid};
use coldbench::engine::{Account, GAP, Meta, SendPlan};
use coldbench::net::{Pins, Pool};
use hww::{Kind, Script};
use std::process::Command;
use std::str::FromStr;

const SEED: &str = "5eb00bbddcf069084889a8ab9155568165f5c453ccb85e70811aaed6f6da5fc19a5ac40b389cd370d086206dec8aa6c43daea6690f20ad3d8d48b2d2ce9e38e4";

fn cli(args: &[&str]) -> String {
    let out = Command::new("docker")
        .args(["exec", "coldbench-regtest-bitcoind", "bitcoin-cli", "-regtest", "-rpcuser=u", "-rpcpassword=p", "-rpcwallet=miner"])
        .args(args)
        .output()
        .expect("docker runs");
    assert!(out.status.success(), "bitcoin-cli {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn mine(n: u32) {
    let to = cli(&["getnewaddress"]);
    cli(&["generatetoaddress", &n.to_string(), &to]);
}

fn seed() -> Xpriv {
    let bytes: Vec<u8> = (0..SEED.len()).step_by(2).map(|i| u8::from_str_radix(&SEED[i..i + 2], 16).unwrap()).collect();
    Xpriv::new_master(Network::Regtest, &bytes).unwrap()
}

/// Esplora takes a few seconds to index what the node has, in the mempool and in blocks. Wait
/// until it has the node's height and every transaction in the node's mempool.
fn settle() {
    let get = |path: &str| {
        Command::new("curl").args(["-s", "-m", "5", &format!("http://127.0.0.1:3002{path}")]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
    };
    for _ in 0..60 {
        let height = cli(&["getblockcount"]);
        let pool: usize = cli(&["getmempoolinfo"]).split("\"size\":").nth(1).and_then(|r| r.split(',').next()).and_then(|n| n.trim().parse().ok()).unwrap_or(0);
        let seen: usize = get("/mempool").split("\"count\":").nth(1).and_then(|r| r.split(',').next()).and_then(|n| n.trim().parse().ok()).unwrap_or(usize::MAX);
        if get("/blocks/tip/height") == height && seen == pool {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    panic!("Esplora did not catch up with the node");
}

fn sync(rt: &tokio::runtime::Runtime, a: &mut Account) {
    settle();
    let pool = rt
        .block_on(Pool::connect(a.meta.servers(), Network::Regtest, a.meta.salt.as_bytes(), 4, Pins::default()))
        .expect("the regtest Esplora server is up on 127.0.0.1:3002");
    let (known, anchored, floor) = a.known();
    let r = rt.block_on(pool.scan(a.scan_request(), GAP, known, anchored, floor, |_| {})).unwrap();
    a.apply_scan(r).unwrap();
}

/// Esplora's address index trails the node by a few seconds, more after a replacement, so sync
/// again until the wallet shows what the node already has, for up to half a minute.
fn sync_until(rt: &tokio::runtime::Runtime, a: &mut Account, what: &str, done: impl Fn(&Account) -> bool) {
    for _ in 0..30 {
        sync(rt, a);
        if done(a) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    panic!("the wallet never showed {what}");
}

fn sign_and_send(rt: &tokio::runtime::Runtime, a: &Account, mut psbt: Psbt) -> Txid {
    psbt.sign(&seed(), &Secp256k1::new()).map_err(|e| format!("{:?}", e.1)).unwrap();
    let tx = a.finalize(&psbt).unwrap();
    let pool = rt
        .block_on(Pool::connect(a.meta.servers(), Network::Regtest, a.meta.salt.as_bytes(), 4, Pins::default()))
        .unwrap();
    rt.block_on(pool.broadcast(&tx, &a.first_input_script(&tx))).unwrap()
}

#[test]
#[ignore = "needs the regtest containers"]
fn a_payment_confirms_and_a_fee_bump_replaces_it() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let secp = Secp256k1::new();
    let m = seed();
    // A account number of its own each run, so a rerun, or the demo wallet on the same chain,
    // never leaves coins on the addresses this one starts with.
    let account = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() % 1_000_000) as u32;
    let path = hww::account_path(Script::Native, Network::Regtest, account);
    let dir = std::env::temp_dir().join(format!("coldbench-regtest-{}", std::process::id()));
    let meta = Meta {
        name: "regtest".into(),
        network: Network::Regtest,
        script: Script::Native,
        kind: Kind::Jade,
        fingerprint: m.fingerprint(&secp).to_string(),
        account,
        xpub: Xpub::from_priv(&secp, &m.derive_priv(&secp, &path).unwrap()).to_string(),
        server: String::new(),
        salt: "00".into(),
        parallel: 4,
        created: 1,
        last_sync: None,
        labels: Default::default(),
    };
    let mut a = Account::create(meta, &dir).unwrap();

    // Money arrives: it is unconfirmed and incoming first, then confirmed.
    let mine_addr = Address::from_str(&cli(&["getnewaddress"])).unwrap().assume_checked();
    let mine_addr_str = mine_addr.to_string();
    let here = a.receive().address;
    cli(&["sendtoaddress", &here, "1.0"]);
    sync(&rt, &mut a);
    let b = a.balance();
    assert_eq!((b.confirmed, b.incoming), (0, 100_000_000), "an arriving payment is incoming, not yet confirmed");
    mine(1);
    sync(&rt, &mut a);
    let b = a.balance();
    assert_eq!((b.confirmed, b.incoming), (100_000_000, 0));

    // Pay out at a low rate, signalling replaceability.
    let plan = |rate| SendPlan {
        recipients: vec![(a.parse_address(&mine_addr_str).unwrap(), 30_000_000)],
        max: false,
        sat_per_vb: rate,
        rbf: true,
        only: None,
        label: "test payment".into(),
        stonewall: false,
    };
    let built = a.build(&plan(2.0)).unwrap();
    let first = sign_and_send(&rt, &a, built.psbt);
    sync(&rt, &mut a);
    let row = a.txs().into_iter().find(|r| r.txid == first).expect("the payment shows");
    assert!(row.height.is_none() && Account::can_bump(&row), "unconfirmed and replaceable");
    // Change coming back is not money being received.
    assert_eq!(a.balance().incoming, 0, "our own change is not incoming");

    // Replace it with a higher fee: the old one is gone, the new one stands in the mempool.
    let bumped = a.bump(first, 10.0).unwrap();
    assert!(bumped.fee > built.fee);
    let second = sign_and_send(&rt, &a, bumped.psbt);
    assert_ne!(first, second);
    sync_until(&rt, &mut a, "the replacement", |a| a.txs().iter().any(|r| r.txid == second));
    let rows = a.txs();
    assert!(rows.iter().any(|r| r.txid == second && r.height.is_none()), "the replacement is pending");
    assert!(!rows.iter().any(|r| r.txid == first), "the replaced one is dropped from the wallet");

    // And it confirms.
    mine(1);
    sync_until(&rt, &mut a, "the confirmation", |a| a.txs().iter().any(|r| r.txid == second && r.height.is_some()));
    let row = a.txs().into_iter().find(|r| r.txid == second).unwrap();
    assert_eq!(row.confirmations, 1);
    assert!(!Account::can_bump(&row), "a confirmed payment cannot be bumped");
    assert_eq!(a.balance().pending, 0);
    std::fs::remove_dir_all(&dir).ok();
}
