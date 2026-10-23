//! Trezor over USB (WebUSB), on top of `trezor-client`.
//!
//! The device may stop to ask for something at almost any call: a button press, a PIN (as a
//! scrambled keypad on the Trezor One, typed on the device on the Model T and later) or a
//! passphrase. `drive` answers each of those through the [`Prompt`].
//!
//! The USB read is blocking with no limit, so a call cannot be interrupted from the host while the
//! device waits for its owner. `cancelled` is checked between messages; to abandon a call that is
//! waiting on the device, press cancel on the device itself.

use crate::{Error, Found, Info, Kind, Notice, Passphrase, Prompt, Result, Script, Signer};
use bitcoin::base64::{Engine, engine::general_purpose::STANDARD};
use bitcoin::bip32::{DerivationPath, Xpub};
use bitcoin::ecdsa;
use bitcoin::{EcdsaSighashType, Network, Psbt};
use trezor_client::protos::failure::FailureType;
use trezor_client::transport::webusb::WebUsbTransport;
use trezor_client::{ButtonRequestType, InputScriptType, Trezor, TrezorMessage, TrezorResponse};

mod sign;

pub fn enumerate() -> Vec<Found> {
    // Not `find_devices`, which also probes for a UDP emulator with a read that never times out.
    let Ok(devices) = WebUsbTransport::find_devices(false) else { return Vec::new() };
    devices
        .into_iter()
        .filter_map(|d| {
            let id = usb_id(&d.to_string())?;
            Some(Found { kind: Kind::Trezor, name: format!("{} on usb {id}", d.model), id })
        })
        .collect()
}

/// `Trezor (transport: WebUSB (1:12)) (debug: false)` to `1:12`.
fn usb_id(display: &str) -> Option<String> {
    let start = display.find("WebUSB (")? + "WebUSB (".len();
    let end = display[start..].find(')')? + start;
    Some(display[start..end].to_string())
}

pub struct Tz {
    dev: Trezor,
    info: Info,
}

impl Tz {
    pub fn open(id: &str, network: Network, prompt: &dyn Prompt) -> Result<Tz> {
        if prompt.cancelled() {
            return Err(Error::cancelled());
        }
        let found = WebUsbTransport::find_devices(false)
            .map_err(|e| Error::Io(e.to_string()))?
            .into_iter()
            .find(|d| usb_id(&d.to_string()).as_deref() == Some(id))
            .ok_or(Error::NotFound)?;
        let mut dev = found.connect().map_err(map)?;
        dev.init_device(None).map_err(map)?;
        let features = dev.features().cloned().ok_or(Error::NotFound)?;
        if features.bootloader_mode() {
            return Err(Error::Unsupported("the Trezor is in bootloader mode, unplug and replug it".into()));
        }
        if !features.initialized() {
            return Err(Error::Uninitialised);
        }

        // The first key request is what makes the device ask for the PIN and passphrase.
        if features.pin_protection() && !features.unlocked() {
            prompt.notify(Notice::PinOnDevice);
        }
        let master = {
            let path: DerivationPath = "m/0'".parse().expect("static path");
            let r = dev.get_public_key(&path, network, false).map_err(map)?;
            drive(r, prompt)?
        };
        prompt.notify(Notice::Working);
        let version = format!(
            "{}.{}.{}",
            features.major_version(),
            features.minor_version(),
            features.patch_version()
        );
        let info = Info {
            kind: Kind::Trezor,
            model: if features.model().is_empty() { dev.model().to_string() } else { format!("Trezor {}", features.model()) },
            firmware: version,
            fingerprint: master.parent_fingerprint,
            label: Some(features.label().to_string()).filter(|l| !l.is_empty()),
            network,
        };
        Ok(Tz { dev, info })
    }
}

impl Signer for Tz {
    fn info(&self) -> &Info {
        &self.info
    }

    fn xpub(&mut self, path: &DerivationPath, prompt: &dyn Prompt) -> Result<Xpub> {
        prompt.notify(Notice::Working);
        let r = self.dev.get_public_key(path, self.info.network, false).map_err(map)?;
        drive(r, prompt)
    }

    fn show_address(
        &mut self,
        script: Script,
        path: &DerivationPath,
        prompt: &dyn Prompt,
    ) -> Result<String> {
        prompt.notify(Notice::Confirm("compare the address on the Trezor, then confirm".into()));
        let r = self.dev.get_address(path, input_type(script), self.info.network, true).map_err(map)?;
        let address = drive(r, prompt)?;
        prompt.notify(Notice::Working);
        Ok(address.to_string())
    }

    fn sign_psbt(&mut self, mut psbt: Psbt, prompt: &dyn Prompt) -> Result<Psbt> {
        prompt.notify(Notice::Confirm("review each output and the fee on the Trezor".into()));
        let network = self.info.network;
        let mut sigs: Vec<(usize, Vec<u8>)> = Vec::new();
        let mut progress = drive(self.dev.sign_tx(&psbt, network).map_err(map)?, prompt)?;
        loop {
            if let Some((i, sig)) = progress.get_signature() {
                sigs.push((i, sig.to_vec()));
            }
            if progress.finished() {
                break;
            }
            if prompt.cancelled() {
                return Err(Error::cancelled());
            }
            let ack = sign::ack(progress.tx_request(), &psbt, network)?;
            progress = drive(progress.ack_msg(ack).map_err(map)?, prompt)?;
        }
        for (i, der) in sigs {
            let input = psbt
                .inputs
                .get_mut(i)
                .ok_or_else(|| Error::Protocol(format!("signature for input {i} which does not exist")))?;
            // The device signs with the one key the PSBT names for the input.
            let Some((pubkey, _)) = input.bip32_derivation.iter().next() else {
                return Err(Error::Psbt(format!("input {i} has no key path to attach the signature to")));
            };
            let signature = bitcoin::secp256k1::ecdsa::Signature::from_der(&der)
                .map_err(|e| Error::Protocol(format!("bad signature from the Trezor: {e}")))?;
            input.partial_sigs.insert(
                bitcoin::PublicKey::new(*pubkey),
                ecdsa::Signature { signature, sighash_type: EcdsaSighashType::All },
            );
        }
        prompt.notify(Notice::Working);
        Ok(psbt)
    }

    fn sign_message(
        &mut self,
        script: Script,
        path: &DerivationPath,
        message: &str,
        prompt: &dyn Prompt,
    ) -> Result<String> {
        prompt.notify(Notice::Confirm("check the message on the Trezor, then confirm".into()));
        let r = self
            .dev
            .sign_message(message.to_string(), path, input_type(script), self.info.network)
            .map_err(map)?;
        let (_, sig) = drive(r, prompt)?;
        prompt.notify(Notice::Working);
        let (id, compact) = sig.serialize_compact();
        let base = match script {
            Script::Legacy => 31,
            Script::Nested => 35,
            Script::Native => 39,
        };
        let mut out = Vec::with_capacity(65);
        out.push(base + id.to_i32() as u8);
        out.extend_from_slice(&compact);
        Ok(STANDARD.encode(out))
    }
}

fn input_type(s: Script) -> InputScriptType {
    match s {
        Script::Legacy => InputScriptType::SPENDADDRESS,
        Script::Nested => InputScriptType::SPENDP2SHWITNESS,
        Script::Native => InputScriptType::SPENDWITNESS,
    }
}

/// Answer whatever the device asks until it gives the result.
fn drive<T, R: TrezorMessage>(mut resp: TrezorResponse<'_, T, R>, prompt: &dyn Prompt) -> Result<T> {
    loop {
        if prompt.cancelled() {
            return Err(Error::cancelled());
        }
        resp = match resp {
            TrezorResponse::Ok(v) => return Ok(v),
            TrezorResponse::Failure(f) => return Err(failure(f.code(), f.message())),
            TrezorResponse::ButtonRequest(r) => {
                if r.request_type() == ButtonRequestType::ButtonRequest_PinEntry {
                    prompt.notify(Notice::PinOnDevice);
                } else if r.request_type() == ButtonRequestType::ButtonRequest_PassphraseEntry {
                    prompt.notify(Notice::PassphraseOnDevice);
                }
                r.ack().map_err(map)?
            }
            TrezorResponse::PinMatrixRequest(r) => {
                prompt.notify(Notice::Working);
                match prompt.pin_matrix() {
                    Some(pin) => r.ack_pin(pin).map_err(map)?,
                    None => return Err(Error::cancelled()),
                }
            }
            TrezorResponse::PassphraseRequest(r) => match prompt.passphrase() {
                Some(Passphrase::Host(p)) => r.ack_passphrase(p).map_err(map)?,
                Some(Passphrase::Device) => {
                    prompt.notify(Notice::PassphraseOnDevice);
                    r.ack(true).map_err(map)?
                }
                None => return Err(Error::cancelled()),
            },
        };
    }
}

fn failure(code: FailureType, message: &str) -> Error {
    match code {
        FailureType::Failure_ActionCancelled | FailureType::Failure_PinCancelled => {
            Error::Declined(if message.is_empty() { "cancelled on the Trezor".into() } else { message.into() })
        }
        FailureType::Failure_PinInvalid | FailureType::Failure_PinExpected | FailureType::Failure_PinMismatch => {
            Error::Locked(if message.is_empty() { "wrong PIN".into() } else { message.into() })
        }
        FailureType::Failure_NotInitialized => Error::Uninitialised,
        FailureType::Failure_Busy => Error::Busy("the Trezor is busy".into()),
        FailureType::Failure_DataError | FailureType::Failure_ProcessError => Error::Psbt(message.into()),
        _ => Error::Protocol(format!("{code:?}: {message}")),
    }
}

fn map(e: trezor_client::Error) -> Error {
    use trezor_client::Error as E;
    match e {
        E::NoDeviceFound | E::DeviceNotUnique => Error::NotFound,
        E::FailureResponse(f) => failure(f.code(), f.message()),
        E::UnsupportedNetwork => Error::Unsupported("the Trezor does not know this network".into()),
        E::InvalidPsbt(m) => Error::Psbt(m),
        E::PsbtMissingInputTx(h) => Error::Psbt(format!("the PSBT lacks the full transaction {h} that an input spends, which the Trezor requires")),
        E::TransportConnect(t) => {
            let m = t.to_string();
            let low = m.to_lowercase();
            if low.contains("access") || low.contains("denied") || low.contains("permission") {
                Error::Busy(format!("{m}; on Linux install 51-trezor.rules, replug, and close Trezor Suite"))
            } else if low.contains("busy") {
                Error::Busy(format!("{m}; close Trezor Suite or any other program using it"))
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
    fn usb_id_comes_out_of_the_display() {
        assert_eq!(
            usb_id("Trezor (transport: WebUSB (1:12)) (debug: false)").as_deref(),
            Some("1:12")
        );
        assert_eq!(usb_id("Trezor Emulator (transport: udp:127.0.0.1:21324)"), None);
    }

    #[test]
    fn cancel_and_pin_failures_are_told_apart() {
        assert!(failure(FailureType::Failure_ActionCancelled, "").is_cancel());
        assert!(matches!(failure(FailureType::Failure_PinInvalid, ""), Error::Locked(_)));
        assert!(matches!(failure(FailureType::Failure_NotInitialized, ""), Error::Uninitialised));
    }
}
