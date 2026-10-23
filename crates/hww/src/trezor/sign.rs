//! Answers to the Trezor's questions while it signs, built from the PSBT.
//!
//! `trezor-client` has its own, but it guesses the script type from the script alone, which makes
//! a P2WPKH change output look like a P2PKH one (the device then refuses it or shows it as a
//! payment) and nested segwit inputs look foreign. Here the type comes from the BIP44, 49 or 84
//! purpose in the key path, which is what the wallet built the output from.

use crate::{Error, Result};
use bitcoin::bip32::{ChildNumber, DerivationPath};
use bitcoin::hashes::Hash;
use bitcoin::{Address, Network, Psbt, Transaction, Txid};
use protobuf::MessageField;
use trezor_client::InputScriptType;
use trezor_client::protos::tx_ack::TransactionType;
use trezor_client::protos::tx_ack::transaction_type::{TxInputType, TxOutputBinType, TxOutputType};
use trezor_client::protos::tx_request::RequestType;
use trezor_client::protos::{OutputScriptType, TxAck, TxRequest};

pub fn ack(req: &TxRequest, psbt: &Psbt, network: Network) -> Result<TxAck> {
    let details = req.details.as_ref().ok_or_else(|| bad("a request without details"))?;
    let index = details.request_index() as usize;
    let prev = if details.has_tx_hash() { Some(previous(psbt, details.tx_hash())?) } else { None };
    let mut tx = TransactionType::new();
    match req.request_type() {
        RequestType::TXMETA => {
            let t = prev.unwrap_or(&psbt.unsigned_tx);
            tx.set_version(t.version.0 as u32);
            tx.set_lock_time(t.lock_time.to_consensus_u32());
            tx.set_inputs_cnt(t.input.len() as u32);
            tx.set_outputs_cnt(t.output.len() as u32);
        }
        RequestType::TXINPUT => tx.inputs.push(match prev {
            Some(t) => prev_input(t, index)?,
            None => input(psbt, index)?,
        }),
        RequestType::TXOUTPUT => match prev {
            Some(t) => {
                let o = t.output.get(index).ok_or_else(|| bad("an output index out of range"))?;
                let mut b = TxOutputBinType::new();
                b.set_amount(o.value.to_sat());
                b.set_script_pubkey(o.script_pubkey.to_bytes());
                tx.bin_outputs.push(b);
            }
            None => tx.outputs.push(output(psbt, index, network)?),
        },
        other => {
            return Err(Error::Unsupported(format!("the Trezor asked for {other:?}, which is not handled")));
        }
    }
    let mut ack = TxAck::new();
    ack.tx = MessageField::some(tx);
    Ok(ack)
}

fn bad(what: &str) -> Error {
    Error::Protocol(format!("the Trezor sent {what}"))
}

/// The full transaction an input spends, which the Trezor checks amounts against.
fn previous<'a>(psbt: &'a Psbt, rev_hash: &[u8]) -> Result<&'a Transaction> {
    let mut b = rev_hash.to_vec();
    b.reverse();
    let txid = Txid::from_slice(&b).map_err(|_| bad("a malformed txid"))?;
    let i = psbt
        .unsigned_tx
        .input
        .iter()
        .position(|i| i.previous_output.txid == txid)
        .ok_or_else(|| bad("a txid this transaction does not spend"))?;
    psbt.inputs[i].non_witness_utxo.as_ref().ok_or_else(|| {
        Error::Psbt(format!(
            "the Trezor needs the full transaction {txid} that input {i} spends, sync the wallet and build it again"
        ))
    })
}

fn prev_input(t: &Transaction, index: usize) -> Result<TxInputType> {
    let i = t.input.get(index).ok_or_else(|| bad("an input index out of range"))?;
    let mut x = TxInputType::new();
    x.set_prev_hash(rev(i.previous_output.txid.to_byte_array()));
    x.set_prev_index(i.previous_output.vout);
    x.set_script_sig(i.script_sig.to_bytes());
    x.set_sequence(i.sequence.to_consensus_u32());
    Ok(x)
}

fn rev(mut b: [u8; 32]) -> Vec<u8> {
    b.reverse();
    b.to_vec()
}

/// The script type a single key path stands for, by its purpose.
fn purpose(path: &DerivationPath) -> Option<u32> {
    match path.into_iter().next()? {
        ChildNumber::Hardened { index } => Some(*index),
        _ => None,
    }
}

fn input(psbt: &Psbt, index: usize) -> Result<TxInputType> {
    let txin = psbt.unsigned_tx.input.get(index).ok_or_else(|| bad("an input index out of range"))?;
    let pin = &psbt.inputs[index];
    let utxo = pin
        .witness_utxo
        .clone()
        .or_else(|| pin.non_witness_utxo.as_ref()?.output.get(txin.previous_output.vout as usize).cloned())
        .ok_or_else(|| Error::Psbt(format!("input {index} does not say what it spends")))?;
    let mut x = TxInputType::new();
    x.set_prev_hash(rev(txin.previous_output.txid.to_byte_array()));
    x.set_prev_index(txin.previous_output.vout);
    x.set_sequence(txin.sequence.to_consensus_u32());
    x.set_amount(utxo.value.to_sat());
    let spk = &utxo.script_pubkey;
    let path = (pin.bip32_derivation.len() == 1).then(|| pin.bip32_derivation.values().next().unwrap().1.clone());
    let kind = match (&path, path.as_ref().and_then(purpose)) {
        (Some(_), _) if spk.is_p2wpkh() => InputScriptType::SPENDWITNESS,
        (Some(_), _) if spk.is_p2pkh() => InputScriptType::SPENDADDRESS,
        (Some(_), Some(49)) if spk.is_p2sh() => InputScriptType::SPENDP2SHWITNESS,
        _ => InputScriptType::EXTERNAL,
    };
    if kind == InputScriptType::EXTERNAL {
        // Not ours: the device needs it signed already.
        x.set_script_pubkey(spk.to_bytes());
        if let Some(s) = &pin.final_script_sig {
            x.set_script_sig(s.to_bytes());
        }
        if let Some(w) = &pin.final_script_witness {
            x.set_witness(bitcoin::consensus::serialize(w));
        }
    }
    if let Some(p) = path {
        x.address_n = p.into_iter().map(|c| u32::from(*c)).collect();
    }
    x.set_script_type(kind);
    Ok(x)
}

fn output(psbt: &Psbt, index: usize, network: Network) -> Result<TxOutputType> {
    let o = psbt.unsigned_tx.output.get(index).ok_or_else(|| bad("an output index out of range"))?;
    let pout = &psbt.outputs[index];
    let mut x = TxOutputType::new();
    x.set_amount(o.value.to_sat());
    if o.script_pubkey.is_op_return() {
        x.set_script_type(OutputScriptType::PAYTOOPRETURN);
        let data = o.script_pubkey.instructions().nth(1).and_then(|i| i.ok()?.push_bytes().map(|b| b.as_bytes().to_vec()));
        x.set_op_return_data(data.unwrap_or_default());
        return Ok(x);
    }
    // Change: ours, one key, a script we know. The device then derives it itself and does not ask.
    let change = (pout.bip32_derivation.len() == 1)
        .then(|| pout.bip32_derivation.values().next().unwrap().1.clone())
        .and_then(|p| {
            let spk = &o.script_pubkey;
            let t = match purpose(&p)? {
                84 if spk.is_p2wpkh() => OutputScriptType::PAYTOWITNESS,
                49 if spk.is_p2sh() => OutputScriptType::PAYTOP2SHWITNESS,
                44 if spk.is_p2pkh() => OutputScriptType::PAYTOADDRESS,
                _ => return None,
            };
            Some((p, t))
        });
    match change {
        Some((p, t)) => {
            x.address_n = p.into_iter().map(|c| u32::from(*c)).collect();
            x.set_script_type(t);
        }
        None => {
            let a = Address::from_script(&o.script_pubkey, network)
                .map_err(|_| Error::Unsupported(format!("output {index} pays a script with no address")))?;
            x.set_address(a.to_string());
            x.set_script_type(OutputScriptType::PAYTOADDRESS);
        }
    }
    Ok(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purpose_is_the_first_hardened_step() {
        assert_eq!(purpose(&"m/84'/0'/0'/1/3".parse().unwrap()), Some(84));
        assert_eq!(purpose(&"m/0/1".parse().unwrap()), None);
    }
}
