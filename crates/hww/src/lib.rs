//! One blocking interface over the hardware wallets this suite supports: Blockstream Jade,
//! Coldcard and Trezor.
//!
//! HWI (the Python tool) is a fine command line, but it is one process per question, it cannot say
//! "type your PIN on the device" while it waits, and it cannot be cancelled once it is waiting. This
//! crate talks to each device directly and gives every blocking call a [`Prompt`], which is how a
//! program learns that the person has something to do on the device and how they take back control.
//!
//! All calls block. Run them on a thread of their own.

mod coldcard;
mod error;
mod jade;
mod prompt;
mod script;
mod trezor;

pub use bitcoin;
pub use error::{Error, Result};
pub use prompt::{Notice, Passphrase, Prompt, Silent};
pub use script::Script;

use bitcoin::bip32::{DerivationPath, Fingerprint, Xpub};
use bitcoin::{Network, Psbt};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    Jade,
    Coldcard,
    Trezor,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Jade, Kind::Coldcard, Kind::Trezor];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Jade => "Jade",
            Kind::Coldcard => "Coldcard",
            Kind::Trezor => "Trezor",
        }
    }

    /// What the person does on the device when it asks for a PIN.
    pub fn pin_help(self) -> &'static str {
        match self {
            Kind::Jade => {
                "Enter the PIN on the Jade. If BIP39 passphrase is on in its Options, it then asks for the passphrase: type it there too, then confirm any prompt it shows."
            }
            Kind::Coldcard => "Enter the PIN on the Coldcard.",
            Kind::Trezor => "Enter the PIN on the Trezor.",
        }
    }
}

/// A device that is plugged in. Finding one does not unlock it or ask it anything about the
/// wallet, so it is safe to do on a timer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Found {
    pub kind: Kind,
    /// Serial port (Jade), serial number (Coldcard) or `bus:address` (Trezor).
    pub id: String,
    /// What the OS calls it, for the list.
    pub name: String,
}

/// What an open device says about itself.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Info {
    pub kind: Kind,
    pub model: String,
    pub firmware: String,
    pub fingerprint: Fingerprint,
    pub label: Option<String>,
    pub network: Network,
}

/// An open, unlocked device.
pub trait Signer: Send {
    fn info(&self) -> &Info;

    /// The extended public key at `path`. Devices that show the path ask the person to confirm it.
    fn xpub(&mut self, path: &DerivationPath, prompt: &dyn Prompt) -> Result<Xpub>;

    /// Show the address at `path` on the device screen and return it, so the person can compare it
    /// with what the computer shows.
    fn show_address(
        &mut self,
        script: Script,
        path: &DerivationPath,
        prompt: &dyn Prompt,
    ) -> Result<String>;

    /// Add this device's signatures to `psbt`. The PSBT is returned with them in `partial_sigs`;
    /// whether it is final is for the caller (or a descriptor wallet) to decide.
    fn sign_psbt(&mut self, psbt: Psbt, prompt: &dyn Prompt) -> Result<Psbt>;

    /// Sign `message` with the key at `path` (BIP137 style, base64 of the 65 byte signature).
    fn sign_message(
        &mut self,
        script: Script,
        path: &DerivationPath,
        message: &str,
        prompt: &dyn Prompt,
    ) -> Result<String>;
}

/// Look for plugged-in devices of every kind. Never prompts, never unlocks.
pub fn enumerate() -> Vec<Found> {
    let mut found = Vec::new();
    found.extend(jade::enumerate());
    found.extend(coldcard::enumerate());
    found.extend(trezor::enumerate());
    found
}

/// Open `device`, unlock it if it needs it (telling `prompt` when the person must act) and read its
/// identity. Fails with [`Error::Declined`] if `prompt.cancelled()` turns true while waiting.
pub fn open(
    device: &Found,
    network: Network,
    prompt: &dyn Prompt,
) -> Result<Box<dyn Signer>> {
    match device.kind {
        Kind::Jade => Ok(Box::new(jade::Jade::open(&device.id, network, prompt)?)),
        Kind::Coldcard => Ok(Box::new(coldcard::Cc::open(&device.id, network, prompt)?)),
        Kind::Trezor => Ok(Box::new(trezor::Tz::open(&device.id, network, prompt)?)),
    }
}

/// `m/84'/0'/0'` for native segwit mainnet account 0 and so on, per BIP44, 49 and 84.
pub fn account_path(script: Script, network: Network, account: u32) -> DerivationPath {
    let coin = if network == Network::Bitcoin { 0 } else { 1 };
    format!("m/{}'/{coin}'/{account}'", script.purpose()).parse().expect("static path")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_paths_follow_bip44_49_84() {
        assert_eq!(account_path(Script::Native, Network::Bitcoin, 0).to_string(), "84'/0'/0'");
        assert_eq!(account_path(Script::Nested, Network::Bitcoin, 2).to_string(), "49'/0'/2'");
        assert_eq!(account_path(Script::Legacy, Network::Testnet, 0).to_string(), "44'/1'/0'");
        assert_eq!(account_path(Script::Native, Network::Signet, 0).to_string(), "84'/1'/0'");
    }
}
