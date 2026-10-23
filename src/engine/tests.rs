use super::*;
use bdk_wallet::bitcoin::bip32::Xpriv;
use bdk_wallet::bitcoin::secp256k1::Secp256k1;
use bdk_wallet::bitcoin::{TxIn, TxOut, Witness, absolute, transaction};

/// The seed of "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon
/// abandon about", the mnemonic every BIP84 vector uses.
const SEED: &str = "5eb00bbddcf069084889a8ab9155568165f5c453ccb85e70811aaed6f6da5fc19a5ac40b389cd370d086206dec8aa6c43daea6690f20ad3d8d48b2d2ce9e38e4";

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn master(network: Network) -> Xpriv {
    Xpriv::new_master(network, &unhex(SEED)).unwrap()
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wallet-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn account(script: Script, network: Network, dir: &Path) -> Account {
    let secp = Secp256k1::new();
    let m = master(network);
    let path = hww::account_path(script, network, 0);
    let xpub = Xpub::from_priv(&secp, &m.derive_priv(&secp, &path).unwrap());
    let meta = Meta {
        name: "test wallet".into(),
        network,
        script,
        kind: Kind::Jade,
        fingerprint: m.fingerprint(&secp).to_string(),
        account: 0,
        xpub: xpub.to_string(),
        server: String::new(),
        salt: "00".into(),
        parallel: 8,
        created: 1,
        last_sync: None,
        labels: BTreeMap::new(),
    };
    Account::create(meta, dir).unwrap()
}

/// Pay `sats` to `address` from nowhere, as if a stranger had sent it.
fn fund(a: &mut Account, address: &str, sats: u64, seen: u64) -> Txid {
    let addr = a.parse_address(address).unwrap();
    let tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::from_raw_hash(bitcoin::hashes::Hash::hash(&seen.to_le_bytes())), 0),
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut { value: Amount::from_sat(sats), script_pubkey: addr.script_pubkey() }],
    };
    let txid = tx.compute_txid();
    a.wallet.apply_unconfirmed_txs([(tx, seen)]);
    a.revision += 1;
    txid
}

#[test]
fn bip84_first_address_matches_the_published_vector() {
    let dir = tmp("vector");
    let mut a = account(Script::Native, Network::Bitcoin, &dir);
    assert_eq!(a.meta.fingerprint, "73c5da0a");
    assert_eq!(a.receive().address, "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu");
    assert_eq!(a.receive().path, "84'/0'/0'/0/0");
}

#[test]
fn bip49_and_bip44_vectors() {
    let dir = tmp("vector2");
    let mut n = account(Script::Nested, Network::Bitcoin, &dir);
    assert_eq!(n.receive().address, "37VucYSaXLCAsxYyAPfbSi9eh4iEcbShgf");
    let mut l = account(Script::Legacy, Network::Bitcoin, &dir);
    assert_eq!(l.receive().address, "1LqBGSKuX5yYUonjxT5qGfpUsXKYYWeabA");
}

#[test]
fn a_wallet_survives_a_restart() {
    let dir = tmp("restart");
    let mut a = account(Script::Native, Network::Bitcoin, &dir);
    let addr = a.receive().address;
    let txid = fund(&mut a, &addr, 50_000, 100);
    a.set_label(&txid.to_string(), "first coins");
    a.save().unwrap();
    let file = a.file().to_path_buf();
    drop(a);

    let b = Account::load(&file).unwrap();
    assert_eq!(b.balance().total(), 50_000);
    assert_eq!(b.txs().len(), 1);
    assert_eq!(b.txs()[0].label, "first coins");
    assert!(b.is_mine(&addr));
}

#[test]
fn history_runs_a_balance() {
    let dir = tmp("history");
    let mut a = account(Script::Native, Network::Bitcoin, &dir);
    let first = a.receive().address;
    let second = a.another().address;
    fund(&mut a, &first, 10_000, 100);
    fund(&mut a, &second, 5_000, 200);
    let rows = a.txs();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].delta, 5_000);
    assert_eq!(rows[0].balance, 15_000);
    assert_eq!(rows[1].balance, 10_000);
}

#[test]
fn a_payment_is_built_signed_and_finalised() {
    let dir = tmp("pay");
    let mut a = account(Script::Native, Network::Bitcoin, &dir);
    let mine = a.receive().address;
    fund(&mut a, &mine, 100_000, 100);

    let to = "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";
    let plan = SendPlan {
        recipients: vec![(a.parse_address(to).unwrap(), 30_000)],
        sat_per_vb: 5.0,
        rbf: true,
        ..Default::default()
    };
    let built = a.build(&plan).unwrap();
    assert_eq!(built.inputs.len(), 1);
    assert_eq!(built.outputs.len(), 2);
    let sent = built.outputs.iter().find(|o| !o.change).unwrap();
    assert_eq!(sent.address, to);
    assert_eq!(sent.value, 30_000);
    let change = built.outputs.iter().find(|o| o.change).unwrap();
    assert_eq!(change.value + sent.value + built.fee, 100_000);
    assert!(a.is_mine(&change.address));
    // About 141 vB at 5 sat/vB.
    assert!((600..=800).contains(&built.fee), "fee {}", built.fee);

    // Sign as a device would, with the key and only the PSBT to go on.
    let secp = Secp256k1::new();
    let mut theirs = built.psbt.clone();
    theirs.sign(&master(Network::Bitcoin), &secp).map_err(|e| format!("{:?}", e.1)).unwrap();
    assert_eq!(Account::signed_inputs(&theirs), 1);

    let mut mine_psbt = built.psbt.clone();
    assert!(a.finalize(&mine_psbt).is_err(), "unsigned must not finalise");
    a.merge(&mut mine_psbt, theirs).unwrap();
    let tx = a.finalize(&mine_psbt).unwrap();
    assert_eq!(tx.input.len(), 1);
    assert!(!tx.input[0].witness.is_empty());
    // The signature in the witness really signs this transaction's BIP143 sighash for that key.
    let spent = built.psbt.inputs[0].witness_utxo.clone().unwrap();
    let w: Vec<&[u8]> = tx.input[0].witness.iter().collect();
    assert_eq!(w.len(), 2, "a P2WPKH witness is a signature and a key");
    let sig = bitcoin::ecdsa::Signature::from_slice(w[0]).unwrap();
    let key = bitcoin::PublicKey::from_slice(w[1]).unwrap();
    let hash = bitcoin::sighash::SighashCache::new(&tx)
        .p2wpkh_signature_hash(0, &spent.script_pubkey, spent.value, sig.sighash_type)
        .unwrap();
    secp.verify_ecdsa(&bitcoin::secp256k1::Message::from_digest(*hash.as_ref()), &sig.signature, &key.inner)
        .expect("signature verifies");
    assert_eq!(ScriptBuf::new_p2wpkh(&key.wpubkey_hash().unwrap()), spent.script_pubkey);
}

#[test]
fn sending_the_lot_leaves_no_change() {
    let dir = tmp("max");
    let mut a = account(Script::Native, Network::Bitcoin, &dir);
    let mine = a.receive().address;
    fund(&mut a, &mine, 80_000, 100);
    let to = a.parse_address("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq").unwrap();
    let built = a
        .build(&SendPlan { recipients: vec![(to, 0)], max: true, sat_per_vb: 2.0, rbf: true, ..Default::default() })
        .unwrap();
    assert_eq!(built.outputs.len(), 1);
    assert_eq!(built.outputs[0].value + built.fee, 80_000);
}

#[test]
fn frozen_coins_are_left_alone() {
    let dir = tmp("freeze");
    let mut a = account(Script::Native, Network::Bitcoin, &dir);
    let first = a.receive().address;
    let second = a.another().address;
    fund(&mut a, &first, 70_000, 100);
    fund(&mut a, &second, 20_000, 200);
    let big = a.utxos().into_iter().find(|u| u.value == 70_000).unwrap().outpoint;
    a.lock(big, true);
    assert!(a.utxos().iter().find(|u| u.outpoint == big).unwrap().frozen);
    let to = a.parse_address("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq").unwrap();
    let plan = SendPlan { recipients: vec![(to, 50_000)], sat_per_vb: 2.0, rbf: true, ..Default::default() };
    let err = a.build(&plan).unwrap_err();
    assert!(err.contains("not enough funds"), "{err}");
    a.lock(big, false);
    assert!(a.build(&plan).is_ok());
}

#[test]
fn bad_input_is_explained_not_panicked_on() {
    let dir = tmp("bad");
    let mut a = account(Script::Native, Network::Bitcoin, &dir);
    assert!(a.parse_address("nonsense").unwrap_err().contains("not a bitcoin address"));
    assert!(a.parse_address("tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx").unwrap_err().contains("another network"));
    let to = a.parse_address("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq").unwrap();
    let plan = SendPlan { recipients: vec![(to.clone(), 1_000)], sat_per_vb: 1.0, rbf: true, ..Default::default() };
    assert!(a.build(&plan).unwrap_err().contains("not enough funds"));
    assert!(a.build(&SendPlan { sat_per_vb: 0.0, ..plan.clone() }).unwrap_err().contains("fee rate"));
    assert!(a.build(&SendPlan::default()).is_err());
}

#[test]
fn a_signature_for_another_transaction_is_refused() {
    let dir = tmp("swap");
    let mut a = account(Script::Native, Network::Bitcoin, &dir);
    let mine = a.receive().address;
    fund(&mut a, &mine, 100_000, 100);
    let to = a.parse_address("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq").unwrap();
    let plan = |v| SendPlan { recipients: vec![(to.clone(), v)], sat_per_vb: 3.0, rbf: true, ..Default::default() };
    let one = a.build(&plan(10_000)).unwrap();
    let two = a.build(&plan(20_000)).unwrap();
    let mut p = one.psbt.clone();
    assert!(a.merge(&mut p, two.psbt).unwrap_err().contains("different transaction"));
}

#[test]
fn message_signatures_verify() {
    // Signed with the first BIP44 key of the test seed.
    let secp = Secp256k1::new();
    let m = master(Network::Bitcoin);
    let sk = m.derive_priv(&secp, &"m/44'/0'/0'/0/0".parse::<DerivationPath>().unwrap()).unwrap().private_key;
    let hash = bitcoin::sign_message::signed_msg_hash("hello");
    let sig = secp.sign_ecdsa_recoverable(&bitcoin::secp256k1::Message::from_digest(*hash.as_ref()), &sk);
    let (id, compact) = sig.serialize_compact();
    let mut raw = vec![31 + id.to_i32() as u8];
    raw.extend_from_slice(&compact);
    use bdk_wallet::bitcoin::base64::{Engine, engine::general_purpose::STANDARD};
    let b64 = STANDARD.encode(raw);
    let address = "1LqBGSKuX5yYUonjxT5qGfpUsXKYYWeabA";
    assert_eq!(verify_message(address, "hello", &b64, Network::Bitcoin), Ok(true));
    assert_eq!(verify_message(address, "hullo", &b64, Network::Bitcoin), Ok(false));
    assert!(verify_message(address, "hello", "AAAA", Network::Bitcoin).is_err());
}

#[test]
fn a_spend_in_the_same_block_follows_what_it_spends() {
    let dir = tmp("order");
    let mut a = account(Script::Native, Network::Bitcoin, &dir);
    let mine = a.receive().address;
    let parent = fund(&mut a, &mine, 50_000, 100);
    let to = a.parse_address("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq").unwrap();
    let built = a.build(&SendPlan { recipients: vec![(to, 0)], max: true, sat_per_vb: 2.0, rbf: true, ..Default::default() }).unwrap();
    let mut psbt = built.psbt.clone();
    psbt.sign(&master(Network::Bitcoin), &Secp256k1::new()).unwrap();
    let tx = a.finalize(&psbt).unwrap();
    // Same first-seen time as its parent.
    a.wallet.apply_unconfirmed_txs([(tx, 100)]);
    let rows = a.txs();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].txid, parent, "oldest is the receive");
    assert!(rows.iter().all(|r| r.balance >= 0), "{:?}", rows.iter().map(|r| r.balance).collect::<Vec<_>>());
}

#[test]
fn an_old_file_gets_a_salt_that_stays() {
    let dir = tmp("salt");
    let a = account(Script::Native, Network::Bitcoin, &dir);
    let file = a.file().to_path_buf();
    drop(a);
    // Make it look like a file from before the salt.
    let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    v["meta"].as_object_mut().unwrap().remove("salt");
    std::fs::write(&file, v.to_string()).unwrap();
    let first = Account::load(&file).unwrap().meta.salt;
    let second = Account::load(&file).unwrap().meta.salt;
    assert_eq!(first, second);
    assert_eq!(first.len(), 32);
}

fn stonewall_wallet(name: &str) -> Account {
    let dir = tmp(name);
    let mut a = account(Script::Native, Network::Bitcoin, &dir);
    for (i, v) in [60_000u64, 55_000, 8_000, 40_000].into_iter().enumerate() {
        let addr = a.another().address;
        fund(&mut a, &addr, v, 100 + i as u64);
    }
    a
}

#[test]
fn a_stonewall_looks_like_two_people_paying_the_same() {
    let mut a = stonewall_wallet("stonewall");
    let to = a.parse_address("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq").unwrap();
    let plan = SendPlan { recipients: vec![(to.clone(), 30_000)], sat_per_vb: 3.0, rbf: true, stonewall: true, ..Default::default() };
    let built = a.build(&plan).unwrap();
    assert!(built.stonewall, "{:?}", built.note);
    assert_eq!(built.outputs.len(), 4);
    assert_eq!(built.outputs.iter().filter(|o| o.value == 30_000).count(), 2, "payment and decoy match");
    assert_eq!(built.outputs.iter().filter(|o| o.decoy).count(), 1);
    assert!(built.outputs.iter().filter(|o| !o.change).all(|o| o.address == to.to_string()));
    let ins: u64 = built.inputs.iter().map(|i| i.value).sum();
    let outs: u64 = built.outputs.iter().map(|o| o.value).sum();
    assert_eq!(ins, outs + built.fee);
    let vb = built.psbt.unsigned_tx.vsize() as f64 + 27.0 * built.inputs.len() as f64;
    assert!((built.fee as f64 - 3.0 * vb).abs() < 40.0, "fee {} for {vb} vB", built.fee);

    let mut p = built.psbt.clone();
    p.sign(&master(Network::Bitcoin), &Secp256k1::new()).unwrap();
    let tx = a.finalize(&p).unwrap();
    assert_eq!(tx.output.len(), 4);
}

#[test]
fn a_stonewall_that_cannot_be_made_falls_back_and_says_why() {
    let mut a = stonewall_wallet("stonewall-fallback");
    let to = a.parse_address("bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq").unwrap();
    // 100k: no two separate halves of the 163k held reach it.
    let plan = SendPlan { recipients: vec![(to, 100_000)], sat_per_vb: 3.0, rbf: true, stonewall: true, ..Default::default() };
    let built = a.build(&plan).unwrap();
    assert!(!built.stonewall);
    assert!(built.note.as_deref().unwrap_or("").contains("not a Stonewall"), "{:?}", built.note);
}

#[test]
fn sparrow_labels_come_in_and_go_back_out() {
    let dir = tmp("labels");
    let mut a = account(Script::Native, Network::Bitcoin, &dir);
    assert_eq!(a.origin(), "wpkh([73c5da0a/84'/0'/0'])");
    let addr = a.receive().address;
    let txid = fund(&mut a, &addr, 50_000, 100);
    let coin = format!("{txid}:0");
    let upper = addr.to_uppercase();
    let file = format!(
        r#"{{"type":"tx","ref":"{txid}","label":"salary","origin":"wpkh([73C5DA0A/84h/0h/0h])"}}
{{"type":"addr","ref":"{upper}","label":"from Alice"}}
{{"type":"output","ref":"{coin}","label":"keep","spendable":false}}
{{"type":"tx","ref":"{txid}","label":"other account","origin":"wpkh([73c5da0a/84'/0'/1'])"}}
{{"type":"tx","ref":"f91d0a8a78462bc59398f2c5d7a84fcff491c26ba54c4833478b202796c8aafd","label":"not ours"}}
{{"type":"xpub","ref":"xpub661MyMwAqRbc","label":"key"}}
garbage
"#
    );
    let got = a.import_labels(&file);
    assert_eq!(got.labels, 3, "{got:?}");
    assert_eq!((got.frozen, got.other_wallet, got.unknown, got.skipped), (1, 1, 1, 1), "{got:?}");
    assert_eq!(got.bad, vec![7]);
    assert_eq!(a.label(&txid.to_string()), "salary");
    assert_eq!(a.label(&addr), "from Alice", "upper case bech32 lands on the address");
    assert_eq!(a.label(&coin), "keep");
    assert!(a.utxos()[0].frozen);

    // Out again, and into a fresh copy of the same wallet: the same labels and the same freeze.
    let out = a.export_labels();
    let (records, bad) = labels::parse(&out);
    assert!(bad.is_empty());
    assert_eq!(records.len(), 3);
    assert!(records.iter().any(|r| r.kind == "output" && r.spendable == Some(false)));
    assert!(records.iter().filter(|r| r.kind == "tx").all(|r| r.origin.as_deref() == Some("wpkh([73c5da0a/84'/0'/0'])")));

    let dir2 = tmp("labels-2");
    let mut b = account(Script::Native, Network::Bitcoin, &dir2);
    let addr2 = b.receive().address;
    assert_eq!(addr2, addr);
    fund(&mut b, &addr2, 50_000, 100);
    let again = b.import_labels(&out);
    assert_eq!((again.labels, again.frozen, again.unknown), (3, 1, 0), "{again:?}");
    assert_eq!(b.label(&txid.to_string()), "salary");

    // A second import changes nothing and says so.
    let twice = b.import_labels(&out);
    assert_eq!((twice.replaced, twice.frozen), (0, 0));
}

#[test]
fn a_sparrow_export_with_its_extra_fields_and_empty_labels() {
    let dir = tmp("labels-sparrow");
    let mut a = account(Script::Native, Network::Bitcoin, &dir);
    let addr = a.receive().address;
    let txid = fund(&mut a, &addr, 50_000, 100);
    let origin = "wpkh([73c5da0a/84'/0'/0']xpub6CatWdiZiodmUeTDp8LT5or8nmbKNcuyvz7WyksVFkKB4RHwCD3XyuvPEbvqAQY3rAPshWcMLoP2fMFMKHPJ4ZeZXYVUhLv1VMrjPC7PW6V/<0;1>/*)#abcdefgh";
    // As Sparrow writes them: origin on everything, extra fields, records with no label.
    let file = format!(
        r#"{{"type":"tx","ref":"{txid}","label":"from the exchange","origin":"{origin}","height":800000,"time":"2023-07-24T01:23:45Z","fee":1410,"value":50000,"rate":{{"USD":29000.5}}}}
{{"type":"addr","ref":"{addr}","origin":"{origin}","keypath":"/0/0","heights":[800000]}}
{{"type":"output","ref":"{txid}:0","label":null,"origin":"{origin}","keypath":"/0/0","value":50000,"height":800000,"time":"2023-07-24T01:23:45Z","fmv":{{"USD":14.5}}}}
{{"type":"input","ref":"{txid}:0","label":"spent here","origin":"{origin}"}}
"#
    );
    let got = a.import_labels(&file);
    assert_eq!(got.labels, 1, "only the one record that has a label: {got:?}");
    assert_eq!(got.bad, Vec::<usize>::new());
    assert_eq!(got.other_wallet, 0, "a full descriptor origin is still ours");
    assert_eq!(a.label(&txid.to_string()), "from the exchange");
    assert_eq!(a.label(&addr), "");
    assert!(!a.utxos()[0].frozen, "no spendable field leaves it as it was");
}

#[test]
fn an_encrypted_wallet_needs_its_password_and_stays_encrypted_when_saved() {
    let dir = tmp("encrypted");
    let mut a = account(Script::Native, Network::Bitcoin, &dir);
    let file = a.file().to_path_buf();
    assert!(!Account::is_locked(&file));
    a.meta.name = "kept".into();
    a.set_password(Some("hunter2")).unwrap();
    assert!(a.encrypted());
    assert!(Account::is_locked(&file));
    let raw = std::fs::read(&file).unwrap();
    assert!(!raw.windows(4).any(|w| w == b"xpub"), "the xpub is not readable in the file");
    assert!(Account::load(&file).is_err());
    assert!(Account::load_with(&file, Some("nope")).is_err());
    let mut b = Account::load_with(&file, Some("hunter2")).unwrap();
    assert_eq!(b.meta.name, "kept");
    assert!(b.password_is("hunter2") && !b.password_is("hunter3"));
    // Saving again keeps it sealed.
    b.meta.name = "again".into();
    b.save().unwrap();
    assert!(Account::is_locked(&file));
    assert_eq!(Account::load_with(&file, Some("hunter2")).unwrap().meta.name, "again");
    // And the password can be taken off.
    b.set_password(None).unwrap();
    assert!(!Account::is_locked(&file));
    assert_eq!(Account::load(&file).unwrap().meta.name, "again");
}
