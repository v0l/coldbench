//! `cargo run --example seed_demo DIR` writes a watch-only wallet of the public BIP84 test seed
//! ("abandon ... about") into DIR/wallets, for looking at the UI without a device.
use bdk_wallet::bitcoin::Network;
use bdk_wallet::bitcoin::bip32::{Xpriv, Xpub};
use bdk_wallet::bitcoin::secp256k1::Secp256k1;
use hww::{Kind, Script};
use wallet::engine::{Account, Meta};

const SEED: &str = "5eb00bbddcf069084889a8ab9155568165f5c453ccb85e70811aaed6f6da5fc19a5ac40b389cd370d086206dec8aa6c43daea6690f20ad3d8d48b2d2ce9e38e4";

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("a folder")).join("wallets");
    let seed: Vec<u8> = (0..SEED.len()).step_by(2).map(|i| u8::from_str_radix(&SEED[i..i + 2], 16).unwrap()).collect();
    let secp = Secp256k1::new();
    let m = Xpriv::new_master(Network::Bitcoin, &seed).unwrap();
    let path = hww::account_path(Script::Native, Network::Bitcoin, 0);
    let xpub = Xpub::from_priv(&secp, &m.derive_priv(&secp, &path).unwrap());
    let meta = Meta {
        name: "test vector".into(),
        network: Network::Bitcoin,
        script: Script::Native,
        kind: Kind::Jade,
        fingerprint: m.fingerprint(&secp).to_string(),
        account: 0,
        xpub: xpub.to_string(),
        server: String::new(),
        created: 1,
        last_sync: None,
        labels: Default::default(),
    };
    let mut a = Account::create(meta.clone(), &dir).unwrap();
    println!("{}", a.file().display());

    // A second copy on the BIP84 account 1 with made-up unconfirmed coins, for the utxos and
    // send pages. Its server is unreachable on purpose so a sync does not wipe the story.
    let path = hww::account_path(Script::Native, Network::Bitcoin, 1);
    let xpub = Xpub::from_priv(&secp, &m.derive_priv(&secp, &path).unwrap());
    let meta = Meta { name: "demo coins".into(), account: 1, xpub: xpub.to_string(), created: 2, server: "http://127.0.0.1:9".into(), ..meta };
    a = Account::create(meta, &dir).unwrap();
    use bdk_wallet::bitcoin::{absolute, transaction, Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness};
    use bdk_wallet::bitcoin::hashes::Hash;
    for (n, (sats, label)) in [(2_500_000u64, "salary"), (840_000, "sold the old bike"), (125_000, ""), (61_200, "refund")].into_iter().enumerate() {
        let to = a.another().address;
        let spk = a.parse_address(&to).unwrap().script_pubkey();
        let tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn { previous_output: OutPoint::new(Txid::hash(&[n as u8]), 0), script_sig: ScriptBuf::new(), sequence: Sequence::MAX, witness: Witness::new() }],
            output: vec![TxOut { value: Amount::from_sat(sats), script_pubkey: spk }],
        };
        a.add_sent(&tx, label).unwrap();
    }
    println!("{}", a.file().display());
}
