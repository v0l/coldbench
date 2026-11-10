//! What a chain analyst can read from a transaction, as plain functions over a
//! `bitcoin::Transaction` and the outputs it spends. Nothing here touches the network.
//!
//! The heuristics are ported from TRACEY (https://github.com/L0laL33tz/TRACEY.diy, MIT licence,
//! `index.html`) and the work it cites:
//!
//! - change detection: an output going back to an input script, the only output of the inputs'
//!   script type, or the only non-round amount (`changeOf`, `fpChangeIndex`);
//! - common input ownership across inputs, with coinjoins left out;
//! - coinjoin detection with the rules of mempool.space's `getTransactionFlags`;
//! - wallet fingerprinting after Ishaana Misra, "Wallet Fingerprints: Detection & Analysis"
//!   (2023) and her `detect_wallet()` (github.com/ishaanam/wallet-fingerprinting): version,
//!   locktime, nSequence, low-R signatures, BIP69 ordering, script types, Taproot and P2WSH
//!   spends, OP_RETURN and address reuse;
//! - exchange and service payouts: one transaction paying many outputs of mixed types.
//!
//! coldbench runs them on the user's own transactions so the user sees what an analyst would
//! infer, before signing.
//!
//! Low-R signatures. Bitcoin Core grinds every ECDSA signature until R is below 0x80, so its
//! DER signatures are never 71 bytes long, and the fingerprint treats a high R as proof the
//! signer was not Core or Electrum. The device that signs decides this, not coldbench, and the
//! review runs on the unsigned PSBT so it cannot see it:
//!
//! - Jade grinds: `sign_psbt` signs with `EC_FLAG_GRIND_R` (Blockstream/Jade, main/process/
//!   sign_psbt.c). Only the anti-exfil path, which takes host entropy, cannot grind.
//! - Coldcard grinds: `psbt.py` retries `ngu.secp256k1.sign(sk, digest, n)` until the DER
//!   length byte is below 0x80, and says it does so to blend in with Core (Coldcard/firmware).
//! - Trezor does not: Bitcoin signing calls `ecdsa_sign_digest` with no canonical check, so
//!   the nonce is plain RFC 6979 and about half of all signatures have a high R
//!   (trezor-firmware, crypto/zkp_ecdsa.c retries only when a caller passes `is_canonical`,
//!   which Ethereum and EOS do).
//!
//! So a Jade or Coldcard spend can stay in the Core and Sparrow set, while a Trezor spend with n
//! inputs shows a high R with probability 1 - 2^-n and then drops Core and Electrum, leaving
//! Sparrow among the survivors. Checked against firmware sources on 2026-11-10.

use bdk_wallet::bitcoin::{ScriptBuf, Transaction, TxOut};
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum ScriptKind {
    P2pkh,
    P2sh,
    P2wpkh,
    P2wsh,
    P2tr,
    OpReturn,
    Other,
}

impl ScriptKind {
    pub fn of(script: &ScriptBuf) -> ScriptKind {
        if script.is_p2pkh() {
            ScriptKind::P2pkh
        } else if script.is_p2sh() {
            ScriptKind::P2sh
        } else if script.is_p2wpkh() {
            ScriptKind::P2wpkh
        } else if script.is_p2wsh() {
            ScriptKind::P2wsh
        } else if script.is_p2tr() {
            ScriptKind::P2tr
        } else if script.is_op_return() {
            ScriptKind::OpReturn
        } else {
            ScriptKind::Other
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ScriptKind::P2pkh => "legacy",
            ScriptKind::P2sh => "nested segwit",
            ScriptKind::P2wpkh => "native segwit",
            ScriptKind::P2wsh => "native script",
            ScriptKind::P2tr => "taproot",
            ScriptKind::OpReturn => "data",
            ScriptKind::Other => "unusual",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Software {
    BitcoinCore,
    Electrum,
    Sparrow,
    BlueWallet,
    TrezorSuite,
    BitBoxApp,
    LedgerLive,
}

impl Software {
    pub const ALL: [Software; 7] = [
        Software::BitcoinCore,
        Software::Electrum,
        Software::Sparrow,
        Software::BlueWallet,
        Software::TrezorSuite,
        Software::BitBoxApp,
        Software::LedgerLive,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Software::BitcoinCore => "Bitcoin Core",
            Software::Electrum => "Electrum",
            Software::Sparrow => "Sparrow",
            Software::BlueWallet => "BlueWallet",
            Software::TrezorSuite => "Trezor Suite",
            Software::BitBoxApp => "BitBoxApp",
            Software::LedgerLive => "Ledger Live",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    pub wallets: Vec<Software>,
    pub why: Vec<String>,
}

impl Fingerprint {
    pub fn names(&self) -> String {
        self.wallets.iter().map(|w| w.name()).collect::<Vec<_>>().join(", ")
    }

    pub fn blends_in(&self) -> bool {
        self.wallets.contains(&Software::BitcoinCore) && self.wallets.contains(&Software::Sparrow)
    }
}

struct Candidates(BTreeSet<Software>);

impl Candidates {
    fn drop(&mut self, gone: &[Software]) {
        for g in gone {
            self.0.remove(g);
        }
    }

    fn only(&mut self, kept: &[Software]) {
        self.0.retain(|w| kept.contains(w));
    }
}

fn spends_consistently(tx: &Transaction, prevouts: &[TxOut]) -> bool {
    !tx.input.is_empty() && tx.input.len() == prevouts.len() && !tx.is_coinbase()
}

fn input_kinds(prevouts: &[TxOut]) -> Vec<ScriptKind> {
    prevouts.iter().map(|p| ScriptKind::of(&p.script_pubkey)).collect()
}

fn output_kinds(tx: &Transaction) -> Vec<ScriptKind> {
    tx.output.iter().map(|o| ScriptKind::of(&o.script_pubkey)).collect()
}

fn is_signed(tx: &Transaction) -> bool {
    tx.input.iter().any(|i| !i.witness.is_empty() || !i.script_sig.is_empty())
}

fn der_r_length(sig: &[u8]) -> Option<u8> {
    (sig.len() > 4 && sig[0] == 0x30).then(|| sig[3])
}

fn first_push(script: &bdk_wallet::bitcoin::Script, nth: usize) -> Option<Vec<u8>> {
    script
        .instructions()
        .filter_map(|i| i.ok())
        .filter_map(|i| i.push_bytes().map(|b| b.as_bytes().to_vec()))
        .nth(nth)
}

fn has_high_r(tx: &Transaction, kinds: &[ScriptKind]) -> bool {
    tx.input.iter().zip(kinds).any(|(i, k)| {
        let sig = match k {
            ScriptKind::P2wpkh => i.witness.nth(0).map(|s| s.to_vec()),
            ScriptKind::P2pkh => first_push(&i.script_sig, 0),
            _ => None,
        };
        sig.and_then(|s| der_r_length(&s)).is_some_and(|r| r > 32)
    })
}

fn has_uncompressed_key(tx: &Transaction, kinds: &[ScriptKind]) -> bool {
    tx.input.iter().zip(kinds).any(|(i, k)| {
        let key = match k {
            ScriptKind::P2wpkh => i.witness.nth(1).map(|s| s.to_vec()),
            ScriptKind::P2pkh => first_push(&i.script_sig, 1),
            _ => None,
        };
        key.is_some_and(|k| k.first() == Some(&4))
    })
}

pub fn follows_bip69(tx: &Transaction) -> (bool, bool) {
    let ins = tx.input.windows(2).all(|w| {
        let a = (w[0].previous_output.txid.to_string(), w[0].previous_output.vout);
        let b = (w[1].previous_output.txid.to_string(), w[1].previous_output.vout);
        a <= b
    });
    let outs = tx.output.windows(2).all(|w| {
        (w[0].value, w[0].script_pubkey.as_bytes()) <= (w[1].value, w[1].script_pubkey.as_bytes())
    });
    (ins, outs)
}

pub fn is_round(sats: u64) -> bool {
    sats % 100_000 == 0
}

pub fn fingerprint_change_index(tx: &Transaction, prevouts: &[TxOut]) -> Option<usize> {
    if tx.output.len() < 2 {
        return None;
    }
    let it = input_kinds(prevouts);
    let ot = output_kinds(tx);
    if it.iter().collect::<HashSet<_>>().len() == 1 {
        let same: Vec<usize> = (0..ot.len()).filter(|&i| ot[i] == it[0]).collect();
        if same.len() == 1 {
            return Some(same[0]);
        }
    }
    let ins: HashSet<&ScriptBuf> = prevouts.iter().map(|p| &p.script_pubkey).collect();
    let shared: Vec<usize> = (0..tx.output.len()).filter(|&i| ins.contains(&tx.output[i].script_pubkey)).collect();
    if shared.len() == 1 {
        return Some(shared[0]);
    }
    let odd: Vec<usize> = (0..tx.output.len()).filter(|&i| tx.output[i].value.to_sat() % 100 != 0).collect();
    (odd.len() == 1).then(|| odd[0])
}

pub fn fingerprint(tx: &Transaction, prevouts: &[TxOut]) -> Option<Fingerprint> {
    use Software::*;
    if !spends_consistently(tx, prevouts) {
        return None;
    }
    let mut p = Candidates(Software::ALL.into_iter().collect());
    let mut why = Vec::new();
    let it = input_kinds(prevouts);
    let ot = output_kinds(tx);
    let signed = is_signed(tx);
    let locktime = tx.lock_time.to_consensus_u32();
    let seqs: Vec<u32> = tx.input.iter().map(|i| i.sequence.0).collect();
    let all = |x: u32| seqs.iter().all(|&q| q == x);

    let sparrow_taproot_seq =
        it.iter().all(|k| *k == ScriptKind::P2tr) && seqs.iter().any(|&q| q > 0 && q < 65536);
    if locktime != 0 {
        why.push("locktime near the block height".to_string());
        p.only(&[BitcoinCore, Electrum, Sparrow]);
    } else {
        why.push("locktime 0".to_string());
        p.drop(&[BitcoinCore, Electrum]);
        if !sparrow_taproot_seq {
            p.drop(&[Sparrow]);
        }
    }

    if signed && has_uncompressed_key(tx, &it) {
        why.push("uncompressed public key".to_string());
        p.0.clear();
    }

    match tx.version.0 {
        1 => {
            why.push("version 1".to_string());
            p.drop(&[BitcoinCore, Electrum, BlueWallet, Sparrow]);
        }
        2 => {
            why.push("version 2".to_string());
            p.drop(&[LedgerLive, TrezorSuite, BitBoxApp]);
        }
        _ => {
            why.push("non-standard version".to_string());
            p.0.clear();
        }
    }

    if signed {
        if has_high_r(tx, &it) {
            why.push("not only low-R signatures".to_string());
            p.drop(&[BitcoinCore, Electrum]);
        } else {
            why.push("low-R signatures only".to_string());
        }
    }

    let sparrow_tr = sparrow_taproot_seq && seqs.iter().filter(|&&q| q != 0xfffffffd).count() == 1;
    if all(0xfffffffd) {
        why.push("nSequence MAX-2 (RBF on)".to_string());
    } else if all(0xfffffffe) {
        why.push("nSequence MAX-1 (RBF off, locktime on)".to_string());
    } else if all(0) {
        why.push("nSequence 0 (RBF on)".to_string());
    } else if all(0x8000_0000) {
        why.push("nSequence 2147483648 (RBF on)".to_string());
    } else if all(0xffff_ffff) {
        why.push("all nSequence final (RBF off)".to_string());
    } else {
        why.push("other nSequence values".to_string());
    }
    if !all(0xfffffffd) {
        p.drop(&[BitcoinCore, Electrum, TrezorSuite, BitBoxApp]);
        if !sparrow_tr {
            p.drop(&[Sparrow]);
        }
    }
    if !all(0x8000_0000) {
        p.drop(&[BlueWallet]);
    }
    if !all(0) {
        p.drop(&[LedgerLive]);
    }

    if ot.contains(&ScriptKind::OpReturn) {
        why.push("OP_RETURN output".to_string());
        p.drop(&[BlueWallet, LedgerLive]);
    }
    if it.contains(&ScriptKind::P2tr) {
        why.push("spends Taproot".to_string());
        p.drop(&[Electrum, BlueWallet, LedgerLive]);
    }
    if it.contains(&ScriptKind::P2wsh) {
        why.push("spends P2WSH".to_string());
        p.drop(&[TrezorSuite, BitBoxApp]);
    }
    if it.iter().collect::<HashSet<_>>().len() > 1 {
        why.push("mixed input types".to_string());
        p.drop(&[Electrum, BlueWallet, LedgerLive, TrezorSuite, Sparrow, BitBoxApp]);
    }

    let change = fingerprint_change_index(tx, prevouts);
    if let Some(ci) = change {
        let ct = ot[ci];
        let rest_has = ot.iter().enumerate().any(|(i, t)| i != ci && *t == ct);
        if rest_has && !it.contains(&ct) {
            why.push("change type matches the payment".to_string());
            p.only(&[BitcoinCore]);
        } else if !rest_has && it.contains(&ct) {
            why.push("change type matches the inputs".to_string());
            p.drop(&[BitcoinCore]);
        }
    }

    let in_scripts: HashSet<&ScriptBuf> = prevouts.iter().map(|p| &p.script_pubkey).collect();
    if tx.output.iter().any(|o| in_scripts.contains(&o.script_pubkey)) {
        why.push("pays back to an input address".to_string());
        p.0.clear();
    } else {
        why.push("no address reuse".to_string());
    }

    if tx.output.len() > 2 {
        why.push("more than 2 outputs".to_string());
        p.drop(&[LedgerLive]);
    }
    let (bip69_in, bip69_out) = follows_bip69(tx);
    if tx.output.len() > 1 {
        if bip69_out {
            why.push("outputs in BIP69 order".to_string());
        } else {
            why.push("outputs not in BIP69 order".to_string());
            p.drop(&[Electrum]);
        }
    }
    if tx.input.len() > 1 {
        if bip69_in {
            why.push("inputs in BIP69 order".to_string());
        } else {
            why.push("inputs not in BIP69 order".to_string());
            p.drop(&[Electrum]);
        }
    }
    match change {
        Some(ci) if ci != tx.output.len() - 1 => {
            why.push("change is not the last output".to_string());
            p.drop(&[LedgerLive, BlueWallet]);
        }
        Some(_) => why.push("change is the last output".to_string()),
        None => {}
    }

    Some(Fingerprint { wallets: p.0.into_iter().collect(), why })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChangeWhy {
    BackToInput,
    SameTypeAsInputs,
    OtherIsRound,
}

impl ChangeWhy {
    pub fn text(self) -> &'static str {
        match self {
            ChangeWhy::BackToInput => "it goes back to an input address",
            ChangeWhy::SameTypeAsInputs => "it has the same address type as the inputs and the other output does not",
            ChangeWhy::OtherIsRound => "the other output is a round amount",
        }
    }
}

pub fn likely_change(tx: &Transaction, prevouts: &[TxOut]) -> Option<(usize, ChangeWhy)> {
    if !spends_consistently(tx, prevouts) {
        return None;
    }
    let outs: Vec<usize> =
        (0..tx.output.len()).filter(|&i| !tx.output[i].script_pubkey.is_op_return()).collect();
    if outs.len() != 2 {
        return None;
    }
    let in_scripts: HashSet<&ScriptBuf> = prevouts.iter().map(|p| &p.script_pubkey).collect();
    let back: Vec<usize> = outs.iter().copied().filter(|&i| in_scripts.contains(&tx.output[i].script_pubkey)).collect();
    if back.len() == 1 {
        return Some((back[0], ChangeWhy::BackToInput));
    }
    if !back.is_empty() {
        return None;
    }
    let in_kinds: HashSet<ScriptKind> = input_kinds(prevouts).into_iter().collect();
    let same: Vec<usize> = outs
        .iter()
        .copied()
        .filter(|&i| in_kinds.len() == 1 && in_kinds.contains(&ScriptKind::of(&tx.output[i].script_pubkey)))
        .collect();
    if same.len() == 1 {
        return Some((same[0], ChangeWhy::SameTypeAsInputs));
    }
    let round: Vec<usize> = outs.iter().copied().filter(|&i| tx.output[i].value.to_sat() % 100_000 == 0).collect();
    if round.len() == 1 {
        let other = *outs.iter().find(|&&i| i != round[0]).expect("two outputs");
        return Some((other, ChangeWhy::OtherIsRound));
    }
    None
}

pub fn is_coinjoin(tx: &Transaction, prevouts: &[TxOut]) -> bool {
    if !spends_consistently(tx, prevouts) {
        return false;
    }
    let (n_in, n_out) = (tx.input.len() as f64, tx.output.len() as f64);
    if n_in < 5.0 || n_out < 5.0 || n_in / n_out >= 5.0 || n_in / n_out <= 0.2 {
        return false;
    }
    let mut seen: HashMap<&ScriptBuf, usize> = HashMap::new();
    for p in prevouts {
        *seen.entry(&p.script_pubkey).or_default() += 1;
    }
    let mut out_counts: HashMap<&ScriptBuf, usize> = HashMap::new();
    for o in &tx.output {
        *out_counts.entry(&o.script_pubkey).or_default() += 1;
    }
    if out_counts.iter().any(|(s, n)| seen.get(s).copied().unwrap_or(0) + n > 1) {
        return false;
    }
    let in_values: HashSet<u64> = prevouts.iter().map(|p| p.value.to_sat()).collect();
    let out_values: HashSet<u64> = tx.output.iter().map(|o| o.value.to_sat()).collect();
    if (in_values.len() + out_values.len()) as f64 > (n_in + n_out) / 2.0 {
        return false;
    }
    if tx.output.iter().any(|o| o.script_pubkey.is_op_return()) {
        return false;
    }
    const ORD: [u8; 6] = [0x00, 0x63, 0x03, 0x6f, 0x72, 0x64];
    let inscription = tx
        .input
        .iter()
        .any(|i| i.witness.iter().any(|w| w.windows(ORD.len()).any(|x| x == ORD)));
    !inscription
}

pub fn is_stonewall_shape(tx: &Transaction) -> bool {
    if tx.input.len() < 2 || tx.output.len() != 4 {
        return false;
    }
    let mut counts: HashMap<u64, usize> = HashMap::new();
    for o in &tx.output {
        *counts.entry(o.value.to_sat()).or_default() += 1;
    }
    counts.len() == 3 && counts.values().any(|&n| n == 2)
}

pub fn payout_shape(tx: &Transaction, threshold: usize) -> Option<usize> {
    let outs: Vec<ScriptKind> = output_kinds(tx).into_iter().filter(|k| *k != ScriptKind::OpReturn).collect();
    let kinds: HashSet<ScriptKind> = outs.iter().copied().collect();
    (outs.len() > threshold && kinds.len() >= 2).then_some(outs.len())
}

pub fn peel_chain_length(chain: &[(Transaction, Vec<TxOut>)]) -> usize {
    let mut run = 0;
    for (i, (tx, prevouts)) in chain.iter().enumerate() {
        if tx.output.len() != 2 || tx.input.len() > 2 || is_coinjoin(tx, prevouts) {
            break;
        }
        let big = tx.output.iter().enumerate().max_by_key(|(_, o)| o.value).map(|(i, _)| i).expect("two outputs");
        let small = tx.output[1 - big].value.to_sat();
        if small * 5 > tx.output[big].value.to_sat() {
            break;
        }
        if let Some((next, _)) = chain.get(i + 1) {
            let txid = tx.compute_txid();
            if !next.input.iter().any(|n| n.previous_output.txid == txid && n.previous_output.vout as usize == big) {
                break;
            }
        }
        run += 1;
    }
    run
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Severity {
    Good,
    Notice,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub text: String,
    pub fix: Option<String>,
}

fn finding(severity: Severity, text: impl Into<String>, fix: Option<&str>) -> Finding {
    Finding { severity, text: text.into(), fix: fix.map(str::to_string) }
}

#[derive(Clone, Debug)]
pub struct Subject<'a> {
    pub tx: &'a Transaction,
    pub prevouts: &'a [TxOut],
    pub own_outputs: &'a [bool],
    pub input_labels: &'a [String],
    pub reused_outputs: &'a [bool],
}

pub fn review(s: &Subject) -> Vec<Finding> {
    let mut out = Vec::new();
    let tx = s.tx;
    if !spends_consistently(tx, s.prevouts) {
        return out;
    }
    let own_output = |i: usize| s.own_outputs.get(i).copied().unwrap_or(false);

    if is_coinjoin(tx, s.prevouts) {
        out.push(finding(Severity::Good, "it looks like a coinjoin, so inputs are not assumed to share an owner", None));
    } else if is_stonewall_shape(tx) {
        out.push(finding(
            Severity::Good,
            "it has the shape of a Stonewall: two equal outputs, so the payment is ambiguous",
            None,
        ));
    } else {
        match likely_change(tx, s.prevouts) {
            Some((i, why)) if own_output(i) => out.push(finding(
                Severity::Warning,
                format!("an analyst picks output {} as your change: {}", i + 1, why.text()),
                Some(match why {
                    ChangeWhy::BackToInput => "send change to a fresh address",
                    ChangeWhy::SameTypeAsInputs => "pay an address of the same type as your coins, or make change of the payment's type",
                    ChangeWhy::OtherIsRound => "pick a non-round amount",
                }),
            )),
            Some((i, _)) => out.push(finding(
                Severity::Good,
                format!("an analyst's change guess is output {}, which is the payment, not your change", i + 1),
                None,
            )),
            None => out.push(finding(Severity::Good, "no clear change output for an analyst to pick", None)),
        }
    }

    match fingerprint(tx, s.prevouts) {
        Some(f) if f.wallets.is_empty() => out.push(finding(
            Severity::Warning,
            "it matches no known wallet software, so it stands out",
            Some("build with version 2, an anti-fee-sniping locktime and shuffled order"),
        )),
        Some(f) if f.blends_in() => out.push(finding(
            Severity::Good,
            format!("wallet fingerprint: indistinguishable from {}", f.names()),
            None,
        )),
        Some(f) => out.push(finding(Severity::Notice, format!("wallet fingerprint narrows to {}", f.names()), None)),
        None => {}
    }

    let labelled: BTreeSet<&str> = s.input_labels.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    if labelled.len() > 1 && !is_coinjoin(tx, s.prevouts) {
        let names: Vec<&str> = labelled.into_iter().collect();
        out.push(finding(
            Severity::Warning,
            format!("the inputs join coins labelled {}: spending them together shows one owner", names.join(", ")),
            Some("use coins from one source"),
        ));
    }

    let payments: Vec<usize> = (0..tx.output.len()).filter(|&i| !own_output(i) && !tx.output[i].script_pubkey.is_op_return()).collect();
    let changes: Vec<usize> = (0..tx.output.len()).filter(|&i| own_output(i)).collect();
    if !changes.is_empty()
        && !payments.is_empty()
        && payments.iter().all(|&i| is_round(tx.output[i].value.to_sat()))
        && changes.iter().all(|&i| !is_round(tx.output[i].value.to_sat()))
    {
        out.push(finding(
            Severity::Notice,
            "the payment is a round amount and the change is not, which marks which is which",
            Some("pick a non-round amount"),
        ));
    }

    let payment_kinds: HashSet<ScriptKind> =
        payments.iter().map(|&i| ScriptKind::of(&tx.output[i].script_pubkey)).collect();
    let change_kinds: HashSet<ScriptKind> =
        changes.iter().map(|&i| ScriptKind::of(&tx.output[i].script_pubkey)).collect();
    if !payment_kinds.is_empty() && !change_kinds.is_empty() && payment_kinds.is_disjoint(&change_kinds) {
        out.push(finding(
            Severity::Notice,
            "your change type differs from the payment type, which points at the change",
            Some("pay an address of your own type, or make change of the payment's type"),
        ));
    }

    let in_scripts: HashSet<&ScriptBuf> = s.prevouts.iter().map(|p| &p.script_pubkey).collect();
    let reuse = (0..tx.output.len()).any(|i| {
        in_scripts.contains(&tx.output[i].script_pubkey) || s.reused_outputs.get(i).copied().unwrap_or(false)
    });
    if reuse {
        out.push(finding(
            Severity::Warning,
            "an output goes to an address that has been used before",
            Some("use a fresh address for each payment and for change"),
        ));
    }

    out.sort_by(|a, b| b.severity.cmp(&a.severity));
    out
}

pub const DUST_RISK: u64 = 1_000;

pub fn is_kyc_label(label: &str) -> bool {
    let l = label.to_lowercase();
    ["kyc", "exchange", "coinbase", "kraken", "binance", "bitstamp", "bitpanda", "strike"].iter().any(|k| l.contains(k))
}

pub fn toxic_change(tx: &Transaction, vout: usize) -> bool {
    if tx.input.len() < 5 || tx.output.len() < 5 {
        return false;
    }
    let mut counts: HashMap<u64, usize> = HashMap::new();
    for o in &tx.output {
        *counts.entry(o.value.to_sat()).or_default() += 1;
    }
    let equal = counts.values().copied().max().unwrap_or(0);
    equal >= 3 && tx.output.get(vout).is_some_and(|o| counts[&o.value.to_sat()] == 1)
}

pub struct CoinFacts<'a> {
    pub value: u64,
    pub address_receipts: usize,
    pub made_by: &'a Transaction,
    pub vout: usize,
    pub parent_labels: &'a [String],
}

pub fn coin_risks(c: &CoinFacts) -> Vec<Finding> {
    let mut out = Vec::new();
    if c.address_receipts > 1 {
        out.push(finding(
            Severity::Warning,
            format!("address reused: it has received {} times, so those payments are linked", c.address_receipts),
            Some("spend it apart from other coins and use a fresh address next time"),
        ));
    }
    if c.value < DUST_RISK {
        out.push(finding(
            Severity::Notice,
            "dust: spending it costs about what it is worth, and a dust attack may be tracking it",
            Some("freeze it instead of spending it"),
        ));
    }
    if toxic_change(c.made_by, c.vout) {
        out.push(finding(
            Severity::Warning,
            "toxic change: left over from a coinjoin, it links back to the mix",
            Some("freeze it, or spend it alone"),
        ));
    }
    let sources: BTreeSet<&str> = c.parent_labels.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    if sources.len() > 1 {
        let kyc = sources.iter().any(|l| is_kyc_label(l));
        let names: Vec<&str> = sources.into_iter().collect();
        out.push(finding(
            if kyc { Severity::Warning } else { Severity::Notice },
            format!("made by merging coins labelled {}{}", names.join(", "), if kyc { ", one of them a KYC source" } else { "" }),
            Some("keep sources apart when spending"),
        ));
    }
    out
}

#[cfg(test)]
mod real;
#[cfg(test)]
mod tests;
