//! `cargo run --example regtest_demo DIR` writes a regtest watch-only wallet of the public BIP84
//! test seed ("abandon ... about") into DIR/wallets and prints its first address, to fund from a
//! local bitcoind. `cargo run --example regtest_demo DIR sign FILE.psbt` signs a PSBT with that
//! seed, standing in for the device, and writes FILE.signed.psbt. The wallet syncs from the
//! Esplora server on 127.0.0.1:3002, which is where regtest looks by default.
use bdk_wallet::bitcoin::bip32::{Xpriv, Xpub};
use bdk_wallet::bitcoin::secp256k1::Secp256k1;
use bdk_wallet::bitcoin::Network;
use coldbench::engine::{Account, Meta};
use hww::{Kind, Script};

const SEED: &str = "5eb00bbddcf069084889a8ab9155568165f5c453ccb85e70811aaed6f6da5fc19a5ac40b389cd370d086206dec8aa6c43daea6690f20ad3d8d48b2d2ce9e38e4";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = std::path::PathBuf::from(args.get(1).expect("a folder"));
    let seed: Vec<u8> = (0..SEED.len()).step_by(2).map(|i| u8::from_str_radix(&SEED[i..i + 2], 16).unwrap()).collect();
    let secp = Secp256k1::new();
    let m = Xpriv::new_master(Network::Regtest, &seed).unwrap();

    if args.get(2).map(String::as_str) == Some("sign") {
        let file = args.get(3).expect("a .psbt file");
        let bytes = std::fs::read(file).unwrap();
        let mut psbt = coldbench::app::parse_psbt(&bytes).unwrap();
        psbt.sign(&m, &secp).map_err(|e| format!("{:?}", e.1)).unwrap();
        let out = file.replace(".psbt", ".signed.psbt");
        std::fs::write(&out, psbt.serialize()).unwrap();
        println!("{out}");
        return;
    }

    let path = hww::account_path(Script::Native, Network::Regtest, 0);
    let xpub = Xpub::from_priv(&secp, &m.derive_priv(&secp, &path).unwrap());
    let meta = Meta {
        name: "regtest".into(),
        network: Network::Regtest,
        script: Script::Native,
        kind: Kind::Jade,
        fingerprint: m.fingerprint(&secp).to_string(),
        account: 0,
        xpub: xpub.to_string(),
        server: String::new(),
        salt: "00".into(),
        parallel: 4,
        created: 1,
        last_sync: None,
        labels: Default::default(),
    };
    let mut a = Account::create(meta, &dir.join("wallets")).unwrap();
    println!("{}", a.receive().address);
}
