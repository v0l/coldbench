use super::*;
use bdk_wallet::bitcoin::absolute::LockTime;
use bdk_wallet::bitcoin::hashes::Hash;
use bdk_wallet::bitcoin::transaction::Version;
use bdk_wallet::bitcoin::{Amount, OutPoint, Sequence, TxIn, Txid, Witness};

fn wpkh(n: u8) -> ScriptBuf {
    let mut b = vec![0x00, 0x14];
    b.extend([n; 20]);
    ScriptBuf::from_bytes(b)
}

fn tr(n: u8) -> ScriptBuf {
    let mut b = vec![0x51, 0x20];
    b.extend([n; 32]);
    ScriptBuf::from_bytes(b)
}

fn sh(n: u8) -> ScriptBuf {
    let mut b = vec![0xa9, 0x14];
    b.extend([n; 20]);
    b.push(0x87);
    ScriptBuf::from_bytes(b)
}

fn pkh(n: u8) -> ScriptBuf {
    let mut b = vec![0x76, 0xa9, 0x14];
    b.extend([n; 20]);
    b.extend([0x88, 0xac]);
    ScriptBuf::from_bytes(b)
}

fn coin(spk: ScriptBuf, sats: u64) -> TxOut {
    TxOut { value: Amount::from_sat(sats), script_pubkey: spk }
}

fn signed_witness(high_r: bool) -> Witness {
    let mut sig = vec![0x30, 0x44, 0x02, if high_r { 0x21 } else { 0x20 }];
    sig.extend([0x11; 32]);
    sig.push(0x01);
    let mut key = vec![0x02];
    key.extend([0x22; 32]);
    Witness::from_slice(&[sig, key])
}

struct Shape {
    version: i32,
    locktime: u32,
    sequence: u32,
    high_r: bool,
}

const CORE: Shape = Shape { version: 2, locktime: 800_000, sequence: 0xffff_fffd, high_r: false };

fn build(shape: &Shape, inputs: &[(u8, TxOut)], outputs: Vec<TxOut>) -> (Transaction, Vec<TxOut>) {
    let tx = Transaction {
        version: Version(shape.version),
        lock_time: LockTime::from_consensus(shape.locktime),
        input: inputs
            .iter()
            .map(|(n, _)| TxIn {
                previous_output: OutPoint::new(Txid::from_byte_array([*n; 32]), 0),
                script_sig: ScriptBuf::new(),
                sequence: Sequence(shape.sequence),
                witness: signed_witness(shape.high_r),
            })
            .collect(),
        output: outputs,
    };
    (tx, inputs.iter().map(|(_, p)| p.clone()).collect())
}

fn names(f: &Fingerprint) -> Vec<Software> {
    f.wallets.clone()
}

fn payment_with_change() -> (Transaction, Vec<TxOut>) {
    build(
        &CORE,
        &[(1, coin(wpkh(1), 250_000))],
        vec![coin(wpkh(9), 120_345), coin(wpkh(2), 129_000)],
    )
}

#[test]
fn core_style_native_segwit_spend_is_core_electrum_or_sparrow() {
    let (tx, prev) = payment_with_change();
    let f = fingerprint(&tx, &prev).unwrap();
    assert!(f.wallets.contains(&Software::BitcoinCore));
    assert!(f.wallets.contains(&Software::Sparrow));
    assert!(!f.wallets.contains(&Software::LedgerLive));
    assert!(f.blends_in());
}

#[test]
fn sparrow_style_spend_with_change_of_other_type_drops_core() {
    let (tx, prev) = build(
        &CORE,
        &[(1, coin(wpkh(1), 250_000))],
        vec![coin(sh(9), 120_345), coin(wpkh(2), 129_000)],
    );
    let f = fingerprint(&tx, &prev).unwrap();
    assert!(!f.wallets.contains(&Software::BitcoinCore));
    assert!(f.wallets.contains(&Software::Sparrow));
}

#[test]
fn ledger_live_is_version_1_locktime_0_sequence_0() {
    let shape = Shape { version: 1, locktime: 0, sequence: 0, high_r: false };
    let (tx, prev) = build(&shape, &[(1, coin(wpkh(1), 250_000))], vec![coin(wpkh(9), 100_000), coin(wpkh(2), 149_000)]);
    assert_eq!(names(&fingerprint(&tx, &prev).unwrap()), vec![Software::LedgerLive]);
}

#[test]
fn bluewallet_is_version_2_locktime_0_sequence_2_pow_31() {
    let shape = Shape { version: 2, locktime: 0, sequence: 0x8000_0000, high_r: false };
    let (tx, prev) = build(&shape, &[(1, coin(wpkh(1), 250_000))], vec![coin(wpkh(2), 149_000), coin(wpkh(9), 100_000)]);
    let f = fingerprint(&tx, &prev).unwrap();
    assert!(f.wallets.contains(&Software::BlueWallet));
    assert!(!f.wallets.contains(&Software::BitcoinCore));
}

#[test]
fn trezor_suite_is_version_1_with_rbf_and_no_locktime() {
    let shape = Shape { version: 1, locktime: 0, sequence: 0xffff_fffd, high_r: false };
    let (tx, prev) = build(&shape, &[(1, coin(wpkh(1), 250_000))], vec![coin(wpkh(9), 100_000), coin(wpkh(2), 149_000)]);
    let f = fingerprint(&tx, &prev).unwrap();
    assert!(f.wallets.contains(&Software::TrezorSuite));
    assert!(f.wallets.contains(&Software::BitBoxApp));
    assert!(!f.wallets.contains(&Software::BitcoinCore));
}

#[test]
fn a_high_r_signature_rules_out_core_and_electrum() {
    let shape = Shape { high_r: true, ..CORE };
    let (tx, prev) = build(&shape, &[(1, coin(wpkh(1), 250_000))], vec![coin(wpkh(9), 120_345), coin(wpkh(2), 129_000)]);
    let f = fingerprint(&tx, &prev).unwrap();
    assert!(!f.wallets.contains(&Software::BitcoinCore));
    assert!(f.wallets.contains(&Software::Sparrow));
}

#[test]
fn paying_back_to_an_input_address_matches_nothing() {
    let (tx, prev) = build(&CORE, &[(1, coin(wpkh(1), 250_000))], vec![coin(wpkh(1), 100_000), coin(wpkh(9), 149_000)]);
    assert!(fingerprint(&tx, &prev).unwrap().wallets.is_empty());
}

#[test]
fn bip69_order_is_detected() {
    let (tx, _) = build(&CORE, &[(1, coin(wpkh(1), 1)), (2, coin(wpkh(2), 1))], vec![coin(wpkh(8), 100), coin(wpkh(9), 200)]);
    assert_eq!(follows_bip69(&tx), (true, true));
    let (tx, _) = build(&CORE, &[(2, coin(wpkh(2), 1)), (1, coin(wpkh(1), 1))], vec![coin(wpkh(9), 200), coin(wpkh(8), 100)]);
    assert_eq!(follows_bip69(&tx), (false, false));
}

#[test]
fn an_unsigned_transaction_is_not_judged_on_signatures() {
    let (mut tx, prev) = payment_with_change();
    for i in &mut tx.input {
        i.witness = Witness::new();
    }
    assert!(fingerprint(&tx, &prev).unwrap().blends_in());
}

#[test]
fn change_is_the_output_of_the_inputs_type_when_the_payment_differs() {
    let (tx, prev) = build(&CORE, &[(1, coin(wpkh(1), 250_000))], vec![coin(tr(9), 100_000), coin(wpkh(2), 149_000)]);
    assert_eq!(likely_change(&tx, &prev), Some((1, ChangeWhy::SameTypeAsInputs)));
}

#[test]
fn change_is_the_output_going_back_to_an_input_address() {
    let (tx, prev) = build(&CORE, &[(1, coin(wpkh(1), 250_000))], vec![coin(wpkh(9), 100_000), coin(wpkh(1), 149_000)]);
    assert_eq!(likely_change(&tx, &prev), Some((1, ChangeWhy::BackToInput)));
}

#[test]
fn change_is_the_other_output_when_one_is_round() {
    let (tx, prev) = build(&CORE, &[(1, coin(wpkh(1), 250_000))], vec![coin(wpkh(9), 100_000), coin(wpkh(2), 149_000)]);
    assert_eq!(likely_change(&tx, &prev), Some((1, ChangeWhy::OtherIsRound)));
}

#[test]
fn no_change_is_named_when_nothing_gives_it_away() {
    let (tx, prev) = build(&CORE, &[(1, coin(wpkh(1), 250_000))], vec![coin(wpkh(9), 100_123), coin(wpkh(2), 149_000)]);
    assert_eq!(likely_change(&tx, &prev), None);
}

fn whirlpool_like() -> (Transaction, Vec<TxOut>) {
    let inputs: Vec<(u8, TxOut)> = (1..=5).map(|n| (n, coin(wpkh(n), 1_000_000 + 1000 * (n as u64 % 2)))).collect();
    let mut outs: Vec<TxOut> = (20..25).map(|n| coin(wpkh(n), 1_000_000)).collect();
    outs.truncate(5);
    build(&CORE, &inputs, outs)
}

#[test]
fn a_five_by_five_coinjoin_with_equal_outputs_is_found() {
    let (tx, prev) = whirlpool_like();
    assert!(is_coinjoin(&tx, &prev));
}

#[test]
fn a_coinjoin_with_an_op_return_is_not() {
    let (mut tx, prev) = whirlpool_like();
    tx.output.push(TxOut { value: Amount::ZERO, script_pubkey: ScriptBuf::new_op_return([1u8; 4]) });
    assert!(!is_coinjoin(&tx, &prev));
}

#[test]
fn a_coinjoin_with_unequal_amounts_is_not() {
    let inputs: Vec<(u8, TxOut)> = (1..=5).map(|n| (n, coin(wpkh(n), 1_000_000 + 7919 * n as u64))).collect();
    let outs: Vec<TxOut> = (20..25).map(|n| coin(wpkh(n), 900_000 + 1234 * n as u64)).collect();
    let (tx, prev) = build(&CORE, &inputs, outs);
    assert!(!is_coinjoin(&tx, &prev));
}

#[test]
fn a_stonewall_shape_is_two_equal_outputs_and_two_changes() {
    let (tx, _) = build(
        &CORE,
        &[(1, coin(wpkh(1), 100_000)), (2, coin(wpkh(2), 100_000))],
        vec![coin(wpkh(8), 50_000), coin(wpkh(9), 50_000), coin(wpkh(3), 48_000), coin(wpkh(4), 47_000)],
    );
    assert!(is_stonewall_shape(&tx));
    let (plain, _) = payment_with_change();
    assert!(!is_stonewall_shape(&plain));
}

#[test]
fn a_batch_payout_of_mixed_types_is_found() {
    let outs: Vec<TxOut> = (0..12u8)
        .map(|n| coin(if n % 3 == 0 { sh(n) } else if n % 3 == 1 { wpkh(n) } else { pkh(n) }, 10_000 + n as u64))
        .collect();
    let (tx, _) = build(&CORE, &[(1, coin(wpkh(1), 5_000_000))], outs);
    assert_eq!(payout_shape(&tx, 10), Some(12));
    let (plain, _) = payment_with_change();
    assert_eq!(payout_shape(&plain, 10), None);
}

#[test]
fn a_peel_chain_is_a_run_of_small_payments_from_the_big_output() {
    let mut chain = Vec::new();
    let mut spk = wpkh(1);
    let mut value = 10_000_000u64;
    let mut prev_txid = Txid::from_byte_array([7; 32]);
    let mut prev_vout = 0u32;
    for step in 0..4u8 {
        let next_spk = wpkh(100 + step);
        let tx = Transaction {
            version: Version(2),
            lock_time: LockTime::from_consensus(800_000),
            input: vec![TxIn {
                previous_output: OutPoint::new(prev_txid, prev_vout),
                script_sig: ScriptBuf::new(),
                sequence: Sequence(0xffff_fffd),
                witness: signed_witness(false),
            }],
            output: vec![coin(wpkh(50 + step), 100_000 + step as u64 * 7), coin(next_spk.clone(), value - 100_000 - 500)],
        };
        let prevouts = vec![coin(spk.clone(), value)];
        prev_txid = tx.compute_txid();
        prev_vout = 1;
        value -= 100_500;
        spk = next_spk;
        chain.push((tx, prevouts));
    }
    assert_eq!(peel_chain_length(&chain), 4);
    chain.swap(1, 2);
    assert!(peel_chain_length(&chain) < 4);
}

fn subject<'a>(
    pair: &'a (Transaction, Vec<TxOut>),
    own: &'a [bool],
    labels: &'a [String],
    reused: &'a [bool],
) -> Subject<'a> {
    Subject { tx: &pair.0, prevouts: &pair.1, own_outputs: own, input_labels: labels, reused_outputs: reused }
}

#[test]
fn merging_coins_with_different_labels_raises_the_cluster_warning() {
    let pair = build(
        &CORE,
        &[(1, coin(wpkh(1), 150_000)), (2, coin(wpkh(2), 150_000))],
        vec![coin(wpkh(9), 120_345), coin(wpkh(3), 170_000)],
    );
    let labels = vec!["exchange".to_string(), "salary".to_string()];
    let found = review(&subject(&pair, &[false, true], &labels, &[]));
    let w = found.iter().find(|f| f.text.contains("join coins labelled")).expect("cluster warning");
    assert_eq!(w.severity, Severity::Warning);
    assert_eq!(w.fix.as_deref(), Some("use coins from one source"));

    let same = vec!["salary".to_string(), "salary".to_string()];
    let found = review(&subject(&pair, &[false, true], &same, &[]));
    assert!(found.iter().all(|f| !f.text.contains("join coins labelled")));
}

#[test]
fn the_review_names_the_change_and_offers_a_fix() {
    let pair = build(&CORE, &[(1, coin(wpkh(1), 250_000))], vec![coin(wpkh(9), 100_000), coin(wpkh(2), 149_000)]);
    let found = review(&subject(&pair, &[false, true], &[String::new()], &[]));
    let c = found.iter().find(|f| f.text.contains("your change")).expect("change finding");
    assert_eq!(c.severity, Severity::Warning);
    assert_eq!(c.fix.as_deref(), Some("pick a non-round amount"));
}

#[test]
fn a_round_payment_beside_odd_change_and_type_mismatch_are_noticed() {
    let pair = build(
        &CORE,
        &[(1, coin(wpkh(1), 250_000))],
        vec![coin(tr(9), 100_000), coin(wpkh(2), 149_000)],
    );
    let found = review(&subject(&pair, &[false, true], &[String::new()], &[]));
    assert!(found.iter().any(|f| f.text.contains("round amount")));
    assert!(found.iter().any(|f| f.text.contains("change type differs")));
}

#[test]
fn address_reuse_is_flagged_from_inputs_and_from_history() {
    let pair = build(&CORE, &[(1, coin(wpkh(1), 250_000))], vec![coin(wpkh(9), 100_123), coin(wpkh(1), 149_000)]);
    let found = review(&subject(&pair, &[false, true], &[String::new()], &[]));
    assert!(found.iter().any(|f| f.text.contains("used before") && f.severity == Severity::Warning));

    let clean = build(&CORE, &[(1, coin(wpkh(1), 250_000))], vec![coin(wpkh(9), 100_123), coin(wpkh(2), 149_000)]);
    let found = review(&subject(&clean, &[false, true], &[String::new()], &[true, false]));
    assert!(found.iter().any(|f| f.text.contains("used before")));
    let found = review(&subject(&clean, &[false, true], &[String::new()], &[false, false]));
    assert!(found.iter().all(|f| !f.text.contains("used before")));
}

#[test]
fn a_coinjoin_review_does_not_assume_one_owner() {
    let pair = whirlpool_like();
    let labels: Vec<String> = ["a", "b", "c", "d", "e"].iter().map(|s| s.to_string()).collect();
    let found = review(&subject(&pair, &[true; 5], &labels, &[]));
    assert!(found.iter().any(|f| f.text.contains("coinjoin") && f.severity == Severity::Good));
    assert!(found.iter().all(|f| !f.text.contains("join coins labelled")));
}
