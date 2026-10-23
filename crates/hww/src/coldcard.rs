//! Coldcard over USB HID, on top of the `coldcard` crate (which does the encrypted session).
//!
//! A Coldcard with a PIN set answers nothing useful until the PIN is typed on its keypad, and it
//! never tells the host when that happens, so opening polls: reopen every half second until the
//! device shows a master key or the person cancels. Signing works the same way: the PSBT is
//! uploaded, the device shows it, and the host polls for the result.

use crate::{Error, Found, Info, Kind, Notice, Prompt, Result, Script, Signer};
use bitcoin::base64::{Engine, engine::general_purpose::STANDARD};
use bitcoin::bip32::{DerivationPath, Xpub};
use bitcoin::{Network, Psbt};
use coldcard::protocol::{AddressFormat, DerivationPath as CcPath, Message};
use coldcard::{Api, Coldcard, SignMode};
use std::str::FromStr;
use std::time::Duration;

const TICK: Duration = Duration::from_millis(400);

pub fn enumerate() -> Vec<Found> {
    let Ok(mut api) = Api::new() else { return Vec::new() };
    let Ok(serials) = api.detect() else { return Vec::new() };
    serials
        .into_iter()
        .map(|sn| Found {
            kind: Kind::Coldcard,
            name: format!("Coldcard {}", sn.as_ref()),
            id: sn.as_ref().to_string(),
        })
        .collect()
}

pub struct Cc {
    dev: Coldcard,
    info: Info,
}

impl Cc {
    pub fn open(serial: &str, network: Network, prompt: &dyn Prompt) -> Result<Cc> {
        let api = Api::new().map_err(map)?;
        let mut told = false;
        let (mut dev, master) = loop {
            if prompt.cancelled() {
                return Err(Error::cancelled());
            }
            let (dev, xpub) = api.open(serial, None).map_err(map)?;
            match xpub {
                Some(x) => break (dev, x),
                None => {
                    // No master key yet: either locked or blank. Blank shows the same way, so
                    // after a few tries say so rather than wait for ever.
                    if !told {
                        prompt.notify(Notice::PinOnDevice);
                        told = true;
                    }
                    drop(dev);
                    std::thread::sleep(TICK);
                }
            }
        };
        prompt.notify(Notice::Working);

        let chain = dev.blockchain().map_err(map)?;
        let device_network = match chain.as_str() {
            "BTC" => Network::Bitcoin,
            "XTN" => Network::Testnet,
            "XRT" => Network::Regtest,
            other => return Err(Error::Protocol(format!("unknown chain {other}"))),
        };
        let ok = match network {
            Network::Bitcoin => device_network == Network::Bitcoin,
            Network::Regtest => device_network == Network::Regtest,
            _ => device_network == Network::Testnet,
        };
        if !ok {
            return Err(Error::Network(format!(
                "this Coldcard is on {} ({chain}), change it under Advanced, Danger Zone, Testnet",
                if device_network == Network::Bitcoin { "mainnet" } else { "testnet" }
            )));
        }

        // The fingerprint the crate hands back is the device's own byte order, so take it from the
        // master key itself, which is what every other tool does.
        let fingerprint = Xpub::from_str(&master.xpub).map_err(Error::from)?.fingerprint();
        let firmware = dev.version().unwrap_or_default().replace('\n', " ").trim().to_string();
        let info = Info {
            kind: Kind::Coldcard,
            model: "Coldcard".into(),
            firmware,
            fingerprint,
            label: None,
            network,
        };
        let _ = &mut dev;
        Ok(Cc { dev, info })
    }

    /// Poll `ask` until it has something or the person gives up.
    fn wait<T>(
        &mut self,
        prompt: &dyn Prompt,
        mut ask: impl FnMut(&mut Coldcard) -> std::result::Result<Option<T>, coldcard::Error>,
    ) -> Result<T> {
        loop {
            if prompt.cancelled() {
                return Err(Error::cancelled());
            }
            if let Some(v) = ask(&mut self.dev).map_err(map)? {
                return Ok(v);
            }
            std::thread::sleep(TICK);
        }
    }
}

impl Signer for Cc {
    fn info(&self) -> &Info {
        &self.info
    }

    fn xpub(&mut self, path: &DerivationPath, prompt: &dyn Prompt) -> Result<Xpub> {
        prompt.notify(Notice::Working);
        let p = cc_path(path)?;
        let s = self.dev.xpub(Some(p)).map_err(map)?;
        Xpub::from_str(&s).map_err(Error::from)
    }

    fn show_address(
        &mut self,
        script: Script,
        path: &DerivationPath,
        prompt: &dyn Prompt,
    ) -> Result<String> {
        prompt.notify(Notice::Confirm("compare the address on the Coldcard".into()));
        let fmt = match script {
            Script::Legacy => AddressFormat::P2PKH,
            Script::Nested => AddressFormat::P2WPKH_P2SH,
            Script::Native => AddressFormat::P2WPKH,
        };
        let address = self.dev.address(cc_path(path)?, fmt).map_err(map)?;
        prompt.notify(Notice::Working);
        Ok(address)
    }

    fn sign_psbt(&mut self, psbt: Psbt, prompt: &dyn Prompt) -> Result<Psbt> {
        prompt.notify(Notice::Working);
        // Signed (flag 4) leaves finishing to us, so the answer is a PSBT with partial signatures.
        self.dev.sign_psbt(&psbt.serialize(), SignMode::Signed).map_err(map)?;
        prompt.notify(Notice::Confirm("review the outputs and fee on the Coldcard, then press OK".into()));
        let bytes = self.wait(prompt, |d| d.get_signed_tx())?;
        prompt.notify(Notice::Working);
        Psbt::deserialize(&bytes).map_err(Error::from)
    }

    fn sign_message(
        &mut self,
        script: Script,
        path: &DerivationPath,
        message: &str,
        prompt: &dyn Prompt,
    ) -> Result<String> {
        prompt.notify(Notice::Working);
        let raw = Message::new(message.as_bytes())
            .map_err(|_| Error::Unsupported("the Coldcard signs messages of 1 to 239 characters".into()))?;
        let fmt = match script {
            Script::Legacy => AddressFormat::P2PKH,
            Script::Nested => AddressFormat::P2WPKH_P2SH,
            Script::Native => AddressFormat::P2WPKH,
        };
        self.dev.sign_message(raw, Some(cc_path(path)?), fmt).map_err(map)?;
        prompt.notify(Notice::Confirm("check the message on the Coldcard, then press OK".into()));
        let signed = self.wait(prompt, |d| d.get_signed_message())?;
        prompt.notify(Notice::Working);
        Ok(STANDARD.encode(signed.signature))
    }
}

fn cc_path(p: &DerivationPath) -> Result<CcPath> {
    let s = if p.is_empty() { "m".to_string() } else { format!("m/{p}") };
    CcPath::new(&s).map_err(|_| Error::Unsupported(format!("the Coldcard cannot use the path {s}")))
}

fn map(e: coldcard::Error) -> Error {
    use coldcard::Error as E;
    use coldcard::protocol::{DecodeError, Response};
    match e {
        E::UnexpectedResponse(Response::Refused) => Error::Declined("refused on the Coldcard".into()),
        E::UnexpectedResponse(Response::Busy) => Error::Busy("the Coldcard is busy, finish what it is showing".into()),
        E::NoSecretOnDevice => Error::Uninitialised,
        E::Decoding(DecodeError::Protocol(m)) => {
            let low = m.to_lowercase();
            if low.contains("pin") || low.contains("lock") {
                Error::Locked(m)
            } else if low.contains("cancel") || low.contains("refus") {
                Error::Declined(m)
            } else {
                Error::Protocol(m)
            }
        }
        E::Hid(h) => {
            let m = h.to_string();
            if m.to_lowercase().contains("denied") || m.to_lowercase().contains("permission") {
                Error::Busy(format!("{m}; on Linux install 51-coinkite.rules and replug"))
            } else if m.to_lowercase().contains("not found") || m.to_lowercase().contains("no such") {
                Error::NotFound
            } else {
                Error::Io(m)
            }
        }
        other => Error::Protocol(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_handed_over_with_an_m() {
        let p: DerivationPath = "84'/0'/0'/0/5".parse().unwrap();
        assert!(cc_path(&p).is_ok());
        assert!(cc_path(&DerivationPath::master()).is_ok());
    }

    #[test]
    fn refusal_reads_as_a_decline() {
        let e = map(coldcard::Error::UnexpectedResponse(coldcard::protocol::Response::Refused));
        assert!(e.is_cancel());
    }
}
