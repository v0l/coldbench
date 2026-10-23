//! Stonewall, the spend Samourai designed and Sparrow builds when "privacy" is picked.
//!
//! One wallet makes a transaction shaped like a coinjoin between two people. Two separate groups
//! of its coins each pay the same amount, one to the recipient and one back to itself as a decoy,
//! and each group gets its own change:
//!
//! ```text
//!   coins A ──┐      ┌── amount    to the recipient
//!             ├──────┼── amount    back to us, the decoy
//!   coins B ──┘      ├── change A
//!                    └── change B
//! ```
//!
//! Someone reading the chain cannot tell which of the two equal outputs is the payment, nor
//! assume every input has one owner. The fee is split between the halves as two people would.
//!
//! Coins on one address always go in the same group: spending them on both sides would show
//! that both groups belong to one wallet.

use super::{Account, Built, SendPlan, explain_build_error};
use bdk_wallet::KeychainKind;
use bdk_wallet::bitcoin::secp256k1::rand::seq::SliceRandom;
use bdk_wallet::bitcoin::secp256k1::rand::thread_rng;
use bdk_wallet::bitcoin::{Amount, FeeRate, OutPoint, ScriptBuf, Sequence};
use hww::Script;
use std::collections::BTreeMap;

/// The smallest output worth making.
const DUST: u64 = 546;
/// Random orderings tried when grouping coins, on top of largest and smallest first.
const TRIES: usize = 48;

/// The coins on one address, always spent together.
#[derive(Clone, Debug)]
pub struct Bundle {
    pub value: u64,
    pub coins: Vec<OutPoint>,
}

/// One side of the split: its coins and what they hold.
pub type Side = (Vec<OutPoint>, u64);

/// Two groups of bundles that each reach `target`, with as few coins and as little left over as
/// the tries find. Bundles are never split between the groups. `None` when no split works.
pub fn split(bundles: &[Bundle], target: u64) -> Option<(Side, Side)> {
    let total: u64 = bundles.iter().map(|b| b.value).sum();
    if total < target.saturating_mul(2) {
        return None;
    }
    let mut by_value: Vec<usize> = (0..bundles.len()).collect();
    by_value.sort_by_key(|&i| std::cmp::Reverse(bundles[i].value));
    let mut orders = vec![by_value.clone()];
    by_value.reverse();
    orders.push(by_value);
    let mut rng = thread_rng();
    for _ in 0..TRIES {
        let mut o: Vec<usize> = (0..bundles.len()).collect();
        o.shuffle(&mut rng);
        orders.push(o);
    }
    // (coins used, value left over), then the bundles on each side.
    type Try = ((usize, u64), Vec<usize>, Vec<usize>);
    let mut best: Option<Try> = None;
    for order in orders {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let (mut va, mut vb) = (0u64, 0u64);
        for i in order {
            if va >= target && vb >= target {
                break;
            }
            // Feed the side that still needs it, the poorer one first.
            if va < target && (vb >= target || va <= vb) {
                a.push(i);
                va += bundles[i].value;
            } else {
                b.push(i);
                vb += bundles[i].value;
            }
        }
        if va < target || vb < target {
            continue;
        }
        let coins: usize = a.iter().chain(&b).map(|&i| bundles[i].coins.len()).sum();
        let score = (coins, (va - target) + (vb - target));
        if best.as_ref().is_none_or(|(s, _, _)| score < *s) {
            best = Some((score, a, b));
        }
    }
    let (_, a, b) = best?;
    let gather = |side: &[usize]| -> Side {
        (side.iter().flat_map(|&i| bundles[i].coins.clone()).collect(), side.iter().map(|&i| bundles[i].value).sum())
    };
    Some((gather(&a), gather(&b)))
}

impl Account {
    pub(super) fn stonewall(&mut self, plan: &SendPlan) -> Result<Built, String> {
        if plan.max || plan.recipients.len() != 1 {
            return Err("it pays one address a set amount, not several or the whole wallet".into());
        }
        let (to, amount) = plan.recipients[0].clone();
        if amount < DUST {
            return Err("the amount is too small".into());
        }
        let rate = plan.sat_per_vb;
        // Rough sizes in vB of an input and an output, to size each half's share of the fee.
        let (vin, vout) = match self.meta.script {
            Script::Native => (68.0, 31.0),
            Script::Nested => (91.0, 32.0),
            Script::Legacy => (148.0, 34.0),
        };

        let mut by_address: BTreeMap<ScriptBuf, Bundle> = BTreeMap::new();
        for u in self.wallet.list_unspent() {
            if self.wallet.is_outpoint_locked(u.outpoint) {
                continue;
            }
            if plan.only.as_ref().is_some_and(|o| !o.contains(&u.outpoint)) {
                continue;
            }
            let b = by_address
                .entry(u.txout.script_pubkey.clone())
                .or_insert(Bundle { value: 0, coins: Vec::new() });
            b.value += u.txout.value.to_sat();
            b.coins.push(u.outpoint);
        }
        let bundles: Vec<Bundle> = by_address.into_values().collect();
        if bundles.len() < 2 {
            return Err("it needs coins on at least two addresses".into());
        }
        // Each half pays the amount, half the fee of a four input, four output transaction (a
        // generous guess, larger splits pay more), and keeps change worth having.
        let half_fee = (rate * (11.0 + 4.0 * vin + 4.0 * vout) / 2.0).ceil() as u64;
        let target = amount + half_fee + DUST;
        let ((a, va), (b, vb)) = split(&bundles, target).ok_or_else(|| {
            "the coins cannot make two separate halves of the amount, each with its share of the fee".to_string()
        })?;

        let decoy = self.wallet.reveal_next_address(KeychainKind::Internal).address.script_pubkey();
        let change_a = self.wallet.reveal_next_address(KeychainKind::Internal).address.script_pubkey();
        let change_b = self.wallet.reveal_next_address(KeychainKind::Internal).address.script_pubkey();
        let all: Vec<OutPoint> = a.iter().chain(&b).copied().collect();
        let fee_rate = FeeRate::from_sat_per_kwu((rate * 250.0).round() as u64);

        // Built twice: first to learn the fee of exactly this shape, then with change A set so
        // each side pays half of it. Same inputs and outputs both times, so the same fee.
        let mut fee = 0u64;
        let mut psbt = None;
        for pass in 0..2 {
            let keep_a = (va - amount)
                .checked_sub(fee.div_ceil(2))
                .ok_or("one side cannot pay its share of the fee")?;
            let mut t = self.wallet.build_tx();
            t.fee_rate(fee_rate);
            if !plan.rbf {
                t.set_exact_sequence(Sequence::ENABLE_LOCKTIME_NO_RBF);
            }
            t.add_utxos(&all).map_err(|e| format!("cannot spend the chosen coins: {e}"))?;
            t.manually_selected_only();
            t.add_recipient(to.script_pubkey(), Amount::from_sat(amount));
            t.add_recipient(decoy.clone(), Amount::from_sat(amount));
            t.add_recipient(change_a.clone(), Amount::from_sat(keep_a));
            t.drain_to(change_b.clone());
            let p = t.finish().map_err(explain_build_error)?;
            if p.unsigned_tx.output.len() != 4 {
                return Err("one side's change would be dust".into());
            }
            let outs: u64 = p.unsigned_tx.output.iter().map(|o| o.value.to_sat()).sum();
            fee = (va + vb).checked_sub(outs).ok_or("the outputs add up to more than the coins")?;
            if pass == 1 {
                psbt = Some(p);
            }
        }
        let psbt = psbt.expect("the second pass keeps its PSBT");
        let kept = |spk: &ScriptBuf| {
            psbt.unsigned_tx
                .output
                .iter()
                .find(|o| o.script_pubkey == *spk)
                .map_or(0, |o| o.value.to_sat())
        };
        if kept(&change_a) < DUST || kept(&change_b) < DUST {
            return Err("one side's change would be dust".into());
        }
        self.revision += 1;
        let decoy_address = self.address_of(&decoy);
        let mut built = self.describe(psbt, rate, plan.label.clone())?;
        for o in &mut built.outputs {
            o.decoy = o.address == decoy_address;
        }
        built.stonewall = true;
        Ok(built)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdk_wallet::bitcoin::Txid;
    use bdk_wallet::bitcoin::hashes::Hash;

    fn bundle(n: u8, value: u64, coins: usize) -> Bundle {
        Bundle { value, coins: (0..coins).map(|i| OutPoint::new(Txid::hash(&[n]), i as u32)).collect() }
    }

    #[test]
    fn halves_each_reach_the_target_and_never_share_an_address() {
        let bundles = vec![bundle(1, 60_000, 1), bundle(2, 50_000, 2), bundle(3, 10_000, 1), bundle(4, 5_000, 1)];
        let ((a, va), (b, vb)) = split(&bundles, 45_000).unwrap();
        assert!(va >= 45_000 && vb >= 45_000);
        assert!(a.iter().all(|o| !b.contains(o)));
        let two = &bundles[1].coins;
        assert!(two.iter().all(|o| a.contains(o)) || two.iter().all(|o| b.contains(o)), "an address stays on one side");
        assert_eq!(a.len() + b.len(), 3, "the two big addresses alone do it");
    }

    #[test]
    fn not_enough_for_two_halves_is_none() {
        assert!(split(&[bundle(1, 100_000, 1), bundle(2, 10_000, 1)], 45_000).is_none(), "one coin cannot be both halves");
        assert!(split(&[bundle(1, 30_000, 1), bundle(2, 30_000, 1)], 40_000).is_none());
    }
}
