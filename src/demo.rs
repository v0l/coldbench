use crate::engine::{Account, Meta};
use bdk_wallet::bitcoin::bip32::{Xpriv, Xpub};
use bdk_wallet::bitcoin::hashes::Hash;
use bdk_wallet::bitcoin::secp256k1::Secp256k1;
use bdk_wallet::bitcoin::{
    Address, Amount, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid,
    Witness, absolute, transaction,
};
use hww::{Kind, Script};
use std::path::{Path, PathBuf};

const SEED: &str = "5eb00bbddcf069084889a8ab9155568165f5c453ccb85e70811aaed6f6da5fc19a5ac40b389cd370d086206dec8aa6c43daea6690f20ad3d8d48b2d2ce9e38e4";

pub const BUSY_PRICE_EUR: f64 = 82_304.526;
pub const BUSY_PRICE_USD: f64 = 82_304.526;
pub const BUSY_INCOMING: u64 = 150 * crate::units::SATS_PER_BTC;
pub const BUSY_COINS: usize = 24;
pub const BUSY_MEMO: &str = "rent for the flat on the quay, march to june, split with the other two, plus the deposit back from the landlord and the share of the broadband nobody paid";

pub fn meta(name: &str, account: u32, created: u64, server: &str) -> Meta {
    let seed: Vec<u8> = (0..SEED.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&SEED[i..i + 2], 16).unwrap())
        .collect();
    let secp = Secp256k1::new();
    let m = Xpriv::new_master(Network::Bitcoin, &seed).unwrap();
    let path = hww::account_path(Script::Native, Network::Bitcoin, account);
    let xpub = Xpub::from_priv(&secp, &m.derive_priv(&secp, &path).unwrap());
    Meta {
        name: name.into(),
        network: Network::Bitcoin,
        script: Script::Native,
        kind: Kind::Jade,
        fingerprint: m.fingerprint(&secp).to_string(),
        account,
        xpub: xpub.to_string(),
        server: server.into(),
        salt: "00".into(),
        parallel: 8,
        created,
        last_sync: None,
        labels: Default::default(),
    }
}

fn pay(from: Vec<OutPoint>, to: Vec<(ScriptBuf, u64)>) -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: from
            .into_iter()
            .map(|o| TxIn {
                previous_output: o,
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::new(),
            })
            .collect(),
        output: to
            .into_iter()
            .map(|(s, v)| TxOut { value: Amount::from_sat(v), script_pubkey: s })
            .collect(),
    }
}

fn stranger(n: u8) -> ScriptBuf {
    ScriptBuf::new_p2wpkh(&bdk_wallet::bitcoin::WPubkeyHash::hash(&[n, 0xee]))
}

fn ours(a: &mut Account) -> ScriptBuf {
    let to = a.another().address;
    a.parse_address(&to).map(|a: Address| a.script_pubkey()).unwrap()
}

pub fn busy(dir: &Path) -> (PathBuf, Txid) {
    let mut a =
        Account::create(meta("busy", 2, 3, "http://127.0.0.1:9"), &dir.join("wallets")).unwrap();
    let mut coins = Vec::new();
    for n in 0..BUSY_COINS {
        let tx = pay(
            vec![OutPoint::new(Txid::hash(&[n as u8, 0xbb]), 0)],
            vec![(ours(&mut a), 200_000 + n as u64 * 1_000)],
        );
        coins.push((OutPoint::new(tx.compute_txid(), 0), tx.output[0].value.to_sat()));
        a.add_sent(&tx, "").unwrap();
    }
    let total: u64 = coins.iter().map(|c| c.1).sum();
    let mut outs: Vec<(ScriptBuf, u64)> =
        (0..BUSY_COINS as u8 - 1).map(|n| (stranger(n), 100_000)).collect();
    let paid: u64 = outs.iter().map(|o| o.1).sum();
    outs.push((a.next_change(), total - paid - 12_000));
    let spend = pay(coins.iter().map(|c| c.0).collect(), outs);
    a.add_sent(&spend, BUSY_MEMO).unwrap();
    let txid = spend.compute_txid();
    let incoming =
        pay(vec![OutPoint::new(Txid::hash(b"whale"), 1)], vec![(ours(&mut a), BUSY_INCOMING)]);
    a.add_sent(&incoming, "").unwrap();
    let rates = serde_json::json!({ "EUR": BUSY_PRICE_EUR, "USD": BUSY_PRICE_USD });
    std::fs::write(dir.join("prices.json"), rates.to_string()).unwrap();
    let config = serde_json::json!({ "fiat": "EUR", "last": a.file() });
    std::fs::write(dir.join("config.json"), config.to_string()).unwrap();
    (a.file().to_path_buf(), txid)
}
