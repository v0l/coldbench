use super::*;
use bdk_wallet::bitcoin::Amount;
use bdk_wallet::bitcoin::consensus::encode::deserialize_hex;

struct Real {
    txid: String,
    tx: Transaction,
    prevouts: Vec<TxOut>,
}

fn load(text: &str) -> Real {
    let mut lines = text.lines();
    let txid = lines.next().unwrap().to_string();
    let tx: Transaction = deserialize_hex(lines.next().unwrap()).unwrap();
    assert_eq!(tx.compute_txid().to_string(), txid);
    let prevouts = lines
        .map(|l| {
            let (value, spk) = l.split_once(' ').unwrap();
            TxOut { value: Amount::from_sat(value.parse().unwrap()), script_pubkey: ScriptBuf::from_hex(spk).unwrap() }
        })
        .collect();
    Real { txid, tx, prevouts }
}

macro_rules! fixture {
    ($name:literal) => {
        load(include_str!(concat!("fixtures/", $name, ".txt")))
    };
}

fn wallets(r: &Real) -> Vec<Software> {
    fingerprint(&r.tx, &r.prevouts).unwrap().wallets
}

#[test]
fn a_real_bitcoin_core_sweep_is_core_electrum_or_sparrow() {
    let r = fixture!("core");
    let w = wallets(&r);
    assert!(w.contains(&Software::BitcoinCore), "{}: {w:?}", r.txid);
    assert!(w.contains(&Software::Sparrow));
    assert!(!w.contains(&Software::LedgerLive) && !w.contains(&Software::BlueWallet) && !w.contains(&Software::TrezorSuite));
}

#[test]
fn a_real_electrum_payment_is_electrum() {
    let r = fixture!("electrum");
    let w = wallets(&r);
    assert!(w.contains(&Software::Electrum), "{}: {w:?}", r.txid);
    assert!(!w.contains(&Software::LedgerLive) && !w.contains(&Software::BlueWallet));
}

#[test]
fn real_bluewallet_payments_are_bluewallet_alone() {
    for r in [fixture!("bluewallet"), fixture!("bluewallet2")] {
        assert_eq!(wallets(&r), vec![Software::BlueWallet], "{}", r.txid);
    }
}

#[test]
fn a_real_trezor_payment_is_trezor_suite_or_bitbox() {
    let r = fixture!("trezor");
    assert_eq!(wallets(&r), vec![Software::TrezorSuite, Software::BitBoxApp], "{}", r.txid);
}

#[test]
fn real_ledger_live_payments_are_ledger_live_alone() {
    for r in [fixture!("ledger"), fixture!("ledger2")] {
        assert_eq!(wallets(&r), vec![Software::LedgerLive], "{}", r.txid);
    }
}

#[test]
fn a_real_whirlpool_mix_is_a_coinjoin_and_its_fingerprint_is_not_a_wallets() {
    let r = fixture!("whirlpool");
    assert!(is_coinjoin(&r.tx, &r.prevouts), "{}", r.txid);
    assert!(!is_stonewall_shape(&r.tx));
    let own = vec![true; 5];
    let found = review(&Subject { tx: &r.tx, prevouts: &r.prevouts, own_outputs: &own, input_labels: &[], reused_outputs: &[] });
    assert!(found.iter().any(|f| f.severity == Severity::Good && f.text.contains("coinjoin")));
}

#[test]
fn a_real_ordinary_payment_is_not_a_coinjoin() {
    for r in [fixture!("electrum"), fixture!("ledger"), fixture!("trezor")] {
        assert!(!is_coinjoin(&r.tx, &r.prevouts), "{}", r.txid);
    }
}

#[test]
fn a_real_batch_payout_is_found_and_a_normal_payment_is_not() {
    let r = fixture!("batch");
    assert_eq!(payout_shape(&r.tx, 10), Some(14), "{}", r.txid);
    assert_eq!(payout_shape(&fixture!("electrum").tx, 10), None);
}

#[test]
fn a_real_peel_chain_is_followed_hop_by_hop() {
    let chain: Vec<(Transaction, Vec<TxOut>)> = [
        fixture!("peel0"),
        fixture!("peel1"),
        fixture!("peel2"),
        fixture!("peel3"),
        fixture!("peel4"),
        fixture!("peel5"),
    ]
    .into_iter()
    .map(|r| (r.tx, r.prevouts))
    .collect();
    assert_eq!(peel_chain_length(&chain), 6);
    let mut broken = chain.clone();
    broken.swap(1, 3);
    assert!(peel_chain_length(&broken) < 6);
}

#[test]
fn change_guesses_on_real_payments_match_the_wallets_rules() {
    let r = fixture!("electrum");
    let guess = likely_change(&r.tx, &r.prevouts);
    assert!(guess.is_some(), "{}", r.txid);
}
