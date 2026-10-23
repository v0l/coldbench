//! Work that waits: devices and servers. Each job runs on its own thread and reports through a
//! channel the UI drains every frame, so the window never blocks and a PIN prompt on a device can
//! be walked away from.

use crate::net::Fees;
use bdk_wallet::KeychainKind;
use bdk_wallet::bitcoin::bip32::{DerivationPath, Fingerprint, Xpub};
use bdk_wallet::bitcoin::{Network, Psbt, Transaction, Txid};
use bdk_wallet::chain::spk_client::{FullScanRequest, FullScanResponse};
use hww::{Found, Info, Kind, Notice, Passphrase, Prompt, Script};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::Arc;
use std::time::Duration;

/// A question for the person, answered by sending on the channel. Dropping it cancels.
pub enum Ask {
    PinMatrix(Sender<Option<String>>),
    Passphrase(Sender<Option<Passphrase>>),
}

pub enum Event {
    Notice(u64, Notice),
    Ask(u64, Ask),
    Done(u64, Outcome),
}

pub enum Outcome {
    Identified { info: Info, xpub: Xpub },
    Address(String),
    Signed(Psbt),
    Message { address: String, signature: String },
    Scanned(PathBuf, Box<FullScanResponse<KeychainKind>>),
    Fees(Fees),
    Broadcast(Txid),
    Devices(Vec<Found>),
    Cancelled,
    Failed(String),
}

/// What to do once the device is open and is the right one.
pub enum Op {
    /// Read the account key the wallet is made from.
    Identify { script: Script, account: u32 },
    Show { script: Script, path: DerivationPath },
    Sign(Psbt),
    Message { script: Script, path: DerivationPath, address: String, text: String },
}

#[derive(Clone)]
pub struct Hub {
    tx: Sender<Event>,
    ctx: egui::Context,
}

pub struct Inbox {
    rx: Receiver<Event>,
}

impl Inbox {
    pub fn drain(&self) -> Vec<Event> {
        self.rx.try_iter().collect()
    }
}

pub fn hub(ctx: &egui::Context) -> (Hub, Inbox) {
    let (tx, rx) = channel();
    (Hub { tx, ctx: ctx.clone() }, Inbox { rx })
}

/// Set from the UI to give up on a running job.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

struct Link {
    id: u64,
    hub: Hub,
    cancel: Cancel,
}

impl Link {
    fn send(&self, e: Event) {
        let _ = self.hub.tx.send(e);
        self.hub.ctx.request_repaint();
    }

    fn wait<T>(&self, rx: Receiver<T>) -> Option<T> {
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(v) => return Some(v),
                Err(RecvTimeoutError::Timeout) if !self.cancel.is_cancelled() => {}
                Err(_) => return None,
            }
        }
    }
}

impl Prompt for Link {
    fn notify(&self, notice: Notice) {
        self.send(Event::Notice(self.id, notice));
    }

    fn pin_matrix(&self) -> Option<String> {
        let (tx, rx) = channel();
        self.send(Event::Ask(self.id, Ask::PinMatrix(tx)));
        self.wait(rx).flatten()
    }

    fn passphrase(&self) -> Option<Passphrase> {
        let (tx, rx) = channel();
        self.send(Event::Ask(self.id, Ask::Passphrase(tx)));
        self.wait(rx).flatten()
    }

    fn cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

impl Hub {
    fn spawn(&self, id: u64, cancel: Cancel, job: impl FnOnce(&Link) -> Outcome + Send + 'static) {
        let link = Link { id, hub: self.clone(), cancel };
        std::thread::spawn(move || {
            let outcome = job(&link);
            link.send(Event::Done(id, outcome));
        });
    }

    pub fn scan_devices(&self, id: u64) {
        self.spawn(id, Cancel::default(), |_| Outcome::Devices(hww::enumerate()));
    }

    /// Find a device (the one given, or any of `kind`), open and unlock it, check it holds the
    /// wallet's keys when `expect` is set, and do `op` on it.
    #[allow(clippy::too_many_arguments)]
    pub fn device(
        &self,
        id: u64,
        cancel: Cancel,
        chosen: Option<Found>,
        kind: Option<Kind>,
        network: Network,
        expect: Option<Fingerprint>,
        op: Op,
    ) {
        self.spawn(id, cancel, move |link| {
            let candidates: Vec<Found> = match chosen {
                Some(f) => vec![f],
                None => hww::enumerate().into_iter().filter(|f| Some(f.kind) == kind || kind.is_none()).collect(),
            };
            if candidates.is_empty() {
                let what = kind.map(|k| k.label()).unwrap_or("hardware wallet");
                return Outcome::Failed(format!("no {what} found, plug it in and try again"));
            }
            let mut wrong = Vec::new();
            for found in candidates {
                let mut signer = match hww::open(&found, network, link) {
                    Ok(s) => s,
                    Err(e) if e.is_cancel() => return Outcome::Cancelled,
                    Err(e) => return Outcome::Failed(e.to_string()),
                };
                let fp = signer.info().fingerprint;
                if expect.is_some_and(|x| x != fp) {
                    wrong.push(fp.to_string());
                    continue;
                }
                let r = match &op {
                    Op::Identify { script, account } => {
                        let path = hww::account_path(*script, network, *account);
                        signer
                            .xpub(&path, link)
                            .map(|xpub| Outcome::Identified { info: signer.info().clone(), xpub })
                    }
                    Op::Show { script, path } => {
                        signer.show_address(*script, path, link).map(Outcome::Address)
                    }
                    Op::Sign(psbt) => signer.sign_psbt(psbt.clone(), link).map(Outcome::Signed),
                    Op::Message { script, path, address, text } => signer
                        .sign_message(*script, path, text, link)
                        .map(|signature| Outcome::Message { address: address.clone(), signature }),
                };
                return match r {
                    Ok(o) => o,
                    Err(e) if e.is_cancel() && link.cancel.is_cancelled() => Outcome::Cancelled,
                    Err(e) => Outcome::Failed(e.to_string()),
                };
            }
            Outcome::Failed(format!(
                "the device plugged in holds other keys (fingerprint {}), this wallet was made with {}",
                wrong.join(", "),
                expect.map(|f| f.to_string()).unwrap_or_default()
            ))
        });
    }

    pub fn scan(&self, id: u64, file: PathBuf, url: String, request: FullScanRequest<KeychainKind>) {
        self.spawn(id, Cancel::default(), move |_| match crate::net::scan(&url, request, crate::engine::GAP) {
            Ok(r) => Outcome::Scanned(file, Box::new(r)),
            Err(e) => Outcome::Failed(e),
        });
    }

    pub fn fees(&self, id: u64, url: String) {
        self.spawn(id, Cancel::default(), move |_| match crate::net::fees(&url) {
            Ok(f) => Outcome::Fees(f),
            Err(e) => Outcome::Failed(e),
        });
    }

    pub fn broadcast(&self, id: u64, url: String, tx: Transaction) {
        self.spawn(id, Cancel::default(), move |_| match crate::net::broadcast(&url, &tx) {
            Ok(()) => Outcome::Broadcast(tx.compute_txid()),
            Err(e) => Outcome::Failed(e),
        });
    }
}
