//! The window: wallets on the left, the open wallet's tabs in the middle, dialogs on top.

use crate::engine::{Account, Built, Meta, network_name};
use crate::net::{self, Fees};
use crate::units::{self, Unit};
use crate::work::{self, Ask, Cancel, Event, Hub, Inbox, Op, Outcome};
use bdk_wallet::bitcoin::bip32::Fingerprint;
use bdk_wallet::bitcoin::{Network, OutPoint, Psbt, Transaction, Txid};
use bdk_wallet::KeychainKind;
use egui::{Margin, RichText, Stroke};
use egui_bench::prelude::*;
use hww::{Found, Kind, Notice, Script};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const SIDE_W: f32 = 270.0;
const DEVICE_POLL: Duration = Duration::from_secs(3);
const FEE_POLL: Duration = Duration::from_secs(300);
const NOTICE_FOR: Duration = Duration::from_secs(12);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Transactions,
    Send,
    Receive,
    Addresses,
    Utxos,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FeePick {
    Fast,
    Medium,
    Slow,
    Custom,
}

impl FeePick {
    pub fn label(self) -> &'static str {
        match self {
            FeePick::Fast => "next block",
            FeePick::Medium => "within 3 blocks",
            FeePick::Slow => "within 6 blocks",
            FeePick::Custom => "custom",
        }
    }
}

#[derive(Clone, Default)]
pub struct Recipient {
    pub address: String,
    pub amount: String,
    pub max: bool,
}

pub struct SendForm {
    pub recipients: Vec<Recipient>,
    pub label: String,
    pub fee: FeePick,
    pub custom: String,
    pub rbf: bool,
    /// Coin control: spend only these.
    pub only: Option<Vec<OutPoint>>,
}

impl Default for SendForm {
    fn default() -> Self {
        SendForm {
            recipients: vec![Recipient::default()],
            label: String::new(),
            fee: FeePick::Medium,
            custom: String::new(),
            rbf: true,
            only: None,
        }
    }
}

/// A transaction on its way out: built (or loaded), collecting signatures, then broadcast.
pub struct Review {
    pub built: Built,
    /// Our copy, with every signature gathered so far.
    pub psbt: Psbt,
    pub tx: Option<Transaction>,
    pub error: Option<String>,
    /// Set when this replaces an earlier transaction.
    pub replaces: Option<Txid>,
}

/// What a device job is for, so its outcome lands in the right place.
#[derive(Clone)]
pub enum Purpose {
    Import(NewWallet),
    Show { address: String },
    Sign,
    Message,
}

pub struct DeviceJob {
    pub id: u64,
    pub cancel: Cancel,
    pub purpose: Purpose,
    pub kind: Option<Kind>,
    pub notice: Notice,
    pub ask: Option<Ask>,
    pub pin: String,
    pub passphrase: String,
    pub started: Instant,
}

#[derive(Clone)]
pub struct NewWallet {
    pub name: String,
    pub network: Network,
    pub script: Script,
    pub account: String,
    /// Index into the device list.
    pub device: Option<usize>,
    pub error: Option<String>,
}

impl Default for NewWallet {
    fn default() -> Self {
        NewWallet {
            name: String::new(),
            network: Network::Bitcoin,
            script: Script::Native,
            account: "0".into(),
            device: None,
            error: None,
        }
    }
}

#[derive(Default)]
pub struct MessageTool {
    pub address: String,
    pub message: String,
    pub signature: String,
    pub result: Option<(bool, String)>,
}

pub enum Pending {
    Device,
    Devices,
    Sync(PathBuf),
    Fees,
    Broadcast { file: PathBuf, label: String },
}

#[derive(Serialize, Deserialize, Default)]
struct Config {
    #[serde(default)]
    unit: Unit,
    #[serde(default)]
    last: Option<PathBuf>,
}

/// What the screens show for the open wallet, rebuilt when the wallet changes.
pub struct Views {
    pub key: (usize, u64),
    pub txs: Vec<crate::engine::TxRow>,
    pub utxos: Vec<crate::engine::UtxoRow>,
    pub receive: Vec<crate::engine::AddrRow>,
    pub change: Vec<crate::engine::AddrRow>,
    pub balance: crate::engine::Balance,
}

pub struct App {
    pub hub: Hub,
    inbox: Inbox,
    next: u64,
    pub pending: HashMap<u64, Pending>,

    pub dir: PathBuf,
    pub accounts: Vec<Account>,
    pub current: Option<usize>,
    pub tab: Tab,
    pub unit: Unit,
    pub views: Option<Views>,

    pub devices: Vec<Found>,
    devices_at: Option<Instant>,
    pub fees: Option<Fees>,
    fees_at: Option<(Instant, Network)>,

    pub device: Option<DeviceJob>,
    pub new_wallet: Option<NewWallet>,
    pub message: Option<MessageTool>,
    pub removing: Option<usize>,
    pub browser: Option<(egui_bench::browser::Browser, Browse)>,

    pub send: SendForm,
    pub preview: Option<(String, Result<Built, String>)>,
    pub review: Option<Review>,
    pub bump_rate: String,

    pub selected_tx: Option<Txid>,
    pub selected_addr: Option<String>,
    pub selected_utxos: BTreeSet<OutPoint>,
    pub addr_chain: KeychainKind,
    pub shown_receive: Option<crate::engine::AddrRow>,
    pub label_edit: String,
    /// What `label_edit` is the label of.
    pub label_key: String,
    pub name_edit: String,
    pub server_edit: String,
    pub parallel_edit: u32,

    pub notice: Option<(bool, String, Instant)>,
    /// Addresses checked so far, by running sync.
    pub progress: HashMap<u64, usize>,
    /// How the last sync of each wallet was spread.
    pub reports: HashMap<PathBuf, net::Report>,
    pub load_errors: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Browse {
    LoadPsbt,
    SavePsbt,
}

impl Tab {
    pub fn named(s: &str) -> Option<Tab> {
        Some(match s {
            "transactions" => Tab::Transactions,
            "send" => Tab::Send,
            "receive" => Tab::Receive,
            "addresses" => Tab::Addresses,
            "utxos" => Tab::Utxos,
            "settings" => Tab::Settings,
            _ => return None,
        })
    }
}

impl App {
    pub fn new(ctx: &egui::Context, dir: PathBuf) -> App {
        let (hub, inbox) = work::hub(ctx, Some(dir.join("pins.json")));
        let config: Config = std::fs::read_to_string(dir.join("config.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let mut app = App {
            hub,
            inbox,
            next: 1,
            pending: HashMap::new(),
            dir,
            accounts: Vec::new(),
            current: None,
            tab: Tab::Transactions,
            unit: config.unit,
            views: None,
            devices: Vec::new(),
            devices_at: None,
            fees: None,
            fees_at: None,
            device: None,
            new_wallet: None,
            message: None,
            removing: None,
            browser: None,
            send: SendForm::default(),
            preview: None,
            review: None,
            bump_rate: String::new(),
            selected_tx: None,
            selected_addr: None,
            selected_utxos: BTreeSet::new(),
            addr_chain: KeychainKind::External,
            shown_receive: None,
            label_edit: String::new(),
            label_key: String::new(),
            name_edit: String::new(),
            server_edit: String::new(),
            parallel_edit: 8,
            notice: None,
            progress: HashMap::new(),
            reports: HashMap::new(),
            load_errors: Vec::new(),
        };
        app.load_all();
        let last = config.last.and_then(|p| app.accounts.iter().position(|a| a.file() == p));
        if let Some(i) = last.or(if app.accounts.is_empty() { None } else { Some(0) }) {
            app.select(i);
        }
        app
    }

    pub fn wallets_dir(&self) -> PathBuf {
        self.dir.join("wallets")
    }

    fn load_all(&mut self) {
        let Ok(entries) = std::fs::read_dir(self.wallets_dir()) else { return };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        paths.sort();
        for p in paths {
            match Account::load(&p) {
                Ok(a) => self.accounts.push(a),
                Err(e) => self.load_errors.push(e),
            }
        }
        self.accounts.sort_by_key(|a| a.meta.created);
    }

    pub fn save_config(&self) {
        let config = Config {
            unit: self.unit,
            last: self.current.map(|i| self.accounts[i].file().to_path_buf()),
        };
        let _ = std::fs::create_dir_all(&self.dir);
        if let Ok(t) = serde_json::to_string_pretty(&config) {
            let _ = std::fs::write(self.dir.join("config.json"), t);
        }
    }

    pub fn id(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    pub fn say(&mut self, ok: bool, text: impl Into<String>) {
        self.notice = Some((ok, text.into(), Instant::now()));
    }

    pub fn account(&self) -> Option<&Account> {
        self.current.map(|i| &self.accounts[i])
    }

    pub fn account_mut(&mut self) -> Option<&mut Account> {
        self.current.map(|i| &mut self.accounts[i])
    }

    pub fn select(&mut self, i: usize) {
        self.current = Some(i);
        self.send = SendForm::default();
        self.preview = None;
        self.review = None;
        self.selected_tx = None;
        self.selected_addr = None;
        self.selected_utxos.clear();
        self.shown_receive = None;
        self.server_edit = self.accounts[i].meta.server.clone();
        self.parallel_edit = self.accounts[i].meta.parallel.clamp(1, 32) as u32;
        self.name_edit = self.accounts[i].meta.name.clone();
        self.label_key.clear();
        self.views = None;
        self.save_config();
        if self.accounts[i].meta.last_sync.is_none_or(|t| units::now().saturating_sub(t) > 60) {
            self.sync(i);
        }
    }

    /// What the footer says about where the wallet syncs from.
    pub fn server(&self, a: &Account) -> String {
        let servers = a.meta.servers();
        match (a.meta.spread(), servers.len()) {
            (true, n) => format!("{n} public servers, addresses split"),
            (false, 1) => servers[0].label(),
            (false, n) => format!("{n} of your servers, addresses split"),
        }
    }

    pub fn syncing(&self, file: &Path) -> bool {
        self.pending.values().any(|p| matches!(p, Pending::Sync(f) if f == file))
    }

    pub fn sync(&mut self, i: usize) {
        let file = self.accounts[i].file().to_path_buf();
        if self.syncing(&file) {
            return;
        }
        let a = &self.accounts[i];
        let (known, anchored, floor) = a.known();
        let job = work::SyncJob {
            file: file.clone(),
            servers: a.meta.servers(),
            network: a.network(),
            salt: a.meta.salt.as_bytes().to_vec(),
            parallel: a.meta.parallel.clamp(1, 64),
            request: a.scan_request(),
            known,
            anchored,
            floor,
        };
        let id = self.id();
        self.pending.insert(id, Pending::Sync(file));
        self.progress.insert(id, 0);
        self.hub.scan(id, job);
    }

    /// The views of the open wallet, rebuilt when it changed.
    pub fn refresh_views(&mut self) {
        let Some(i) = self.current else {
            self.views = None;
            return;
        };
        let a = &self.accounts[i];
        let key = (i, a.revision);
        if self.views.as_ref().is_some_and(|v| v.key == key) {
            return;
        }
        self.views = Some(Views {
            key,
            txs: a.txs(),
            utxos: a.utxos(),
            receive: a.addresses(KeychainKind::External),
            change: a.addresses(KeychainKind::Internal),
            balance: a.balance(),
        });
    }

    // ---- devices ----

    pub fn start_device(&mut self, purpose: Purpose, chosen: Option<Found>, op: Op) {
        if self.device.is_some() {
            return;
        }
        let (kind, network, expect) = match &purpose {
            Purpose::Import(n) => (chosen.as_ref().map(|f| f.kind), n.network, None),
            _ => {
                let Some(a) = self.account() else { return };
                (Some(a.meta.kind), a.network(), a.meta.fingerprint())
            }
        };
        let id = self.id();
        let cancel = Cancel::default();
        self.pending.insert(id, Pending::Device);
        self.hub.device(id, cancel.clone(), chosen, kind, network, expect, op);
        self.device = Some(DeviceJob {
            id,
            cancel,
            purpose,
            kind,
            notice: Notice::Working,
            ask: None,
            pin: String::new(),
            passphrase: String::new(),
            started: Instant::now(),
        });
    }

    /// Show the device prompt as it looks while waiting for a PIN, with no device involved.
    /// For screenshots and for checking the look, `wallet --prompt`.
    pub fn preview_prompt(&mut self) {
        self.device = Some(DeviceJob {
            id: 0,
            cancel: Cancel::default(),
            purpose: Purpose::Sign,
            kind: Some(Kind::Jade),
            notice: Notice::PinOnDevice,
            ask: None,
            pin: String::new(),
            passphrase: String::new(),
            started: Instant::now(),
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        for event in self.inbox.drain() {
            match event {
                Event::Progress(id, n) => {
                    self.progress.insert(id, n);
                }
                Event::Notice(id, n) => {
                    if let Some(d) = self.device.as_mut().filter(|d| d.id == id) {
                        d.notice = n;
                    }
                }
                Event::Ask(id, ask) => {
                    if let Some(d) = self.device.as_mut().filter(|d| d.id == id) {
                        d.ask = Some(ask);
                    }
                }
                Event::Done(id, outcome) => {
                    self.progress.remove(&id);
                    let Some(p) = self.pending.remove(&id) else { continue };
                    self.done(p, id, outcome);
                }
            }
        }

        let device_idle = self.device.is_none();
        let polling = !self.pending.values().any(|p| matches!(p, Pending::Devices));
        let want = self.new_wallet.is_some() || self.accounts.is_empty();
        if want
            && device_idle
            && polling
            && self.devices_at.is_none_or(|t| t.elapsed() > DEVICE_POLL)
        {
            self.devices_at = Some(Instant::now());
            let id = self.id();
            self.pending.insert(id, Pending::Devices);
            self.hub.scan_devices(id);
        }
        if want {
            ctx.request_repaint_after(DEVICE_POLL);
        }

        if let Some(a) = self.account() {
            let network = a.network();
            let stale = self.fees_at.is_none_or(|(t, n)| n != network || t.elapsed() > FEE_POLL);
            if stale && !self.pending.values().any(|p| matches!(p, Pending::Fees)) {
                let servers = a.meta.servers();
                self.fees_at = Some((Instant::now(), network));
                let id = self.id();
                self.pending.insert(id, Pending::Fees);
                self.hub.fees(id, servers, network);
            }
        }
        if self.notice.as_ref().is_some_and(|(_, _, t)| t.elapsed() > NOTICE_FOR) {
            self.notice = None;
        }
    }

    fn done(&mut self, pending: Pending, id: u64, outcome: Outcome) {
        match (pending, outcome) {
            (Pending::Devices, Outcome::Devices(found)) => {
                if let Some(n) = &mut self.new_wallet {
                    let picked = n.device.and_then(|i| self.devices.get(i)).cloned();
                    n.device = picked.and_then(|p| found.iter().position(|f| *f == p));
                    if n.device.is_none() && found.len() == 1 {
                        n.device = Some(0);
                    }
                }
                self.devices = found;
            }
            (Pending::Device, outcome) => {
                let Some(job) = self.device.take_if(|d| d.id == id) else { return };
                self.device_done(job.purpose, outcome);
            }
            (Pending::Sync(file), Outcome::Scanned(_, update, report)) => {
                self.reports.insert(file.clone(), report);
                if let Some(a) = self.accounts.iter_mut().find(|a| a.file() == file)
                    && let Err(e) = a.apply_scan(*update) {
                        self.say(false, e);
                    }
            }
            (Pending::Fees, Outcome::Fees(f)) => self.fees = Some(f),
            (Pending::Broadcast { file, label }, Outcome::Broadcast(txid)) => {
                let tx = self.review.as_ref().and_then(|r| r.tx.clone());
                if let Some(a) = self.accounts.iter_mut().find(|a| a.file() == file)
                    && let Some(tx) = tx.filter(|t| t.compute_txid() == txid) {
                        let _ = a.add_sent(&tx, &label);
                    }
                self.review = None;
                self.preview = None;
                self.send = SendForm::default();
                self.selected_tx = Some(txid);
                self.tab = Tab::Transactions;
                self.say(true, format!("sent, transaction {}", units::short(&txid.to_string(), 8)));
                if let Some(i) = self.accounts.iter().position(|a| a.file() == file) {
                    self.sync(i);
                }
            }
            (Pending::Broadcast { .. }, Outcome::Failed(e)) => {
                if let Some(r) = &mut self.review {
                    r.error = Some(e.clone());
                }
                self.say(false, e);
            }
            (Pending::Sync(_), Outcome::Failed(e)) => self.say(false, format!("sync failed: {e}")),
            (Pending::Fees, Outcome::Failed(_)) => {}
            (_, Outcome::Failed(e)) => self.say(false, e),
            _ => {}
        }
    }

    fn device_done(&mut self, purpose: Purpose, outcome: Outcome) {
        match (purpose, outcome) {
            (_, Outcome::Cancelled) => self.say(true, "cancelled"),
            (Purpose::Import(mut n), Outcome::Failed(e)) => {
                n.error = Some(e);
                self.new_wallet = Some(n);
            }
            (_, Outcome::Failed(e)) => self.say(false, e),
            (Purpose::Import(n), Outcome::Identified { info, xpub }) => {
                let name = if n.name.trim().is_empty() {
                    format!("{} {}", info.kind.label(), info.fingerprint)
                } else {
                    n.name.trim().to_string()
                };
                let meta = Meta {
                    name,
                    network: n.network,
                    script: n.script,
                    kind: info.kind,
                    fingerprint: info.fingerprint.to_string(),
                    account: n.account.trim().parse().unwrap_or(0),
                    xpub: xpub.to_string(),
                    server: String::new(),
                    salt: crate::engine::new_salt(),
                    parallel: crate::engine::default_parallel(),
                    created: units::now(),
                    last_sync: None,
                    labels: Default::default(),
                };
                if let Some(i) = self.accounts.iter().position(|a| {
                    a.meta.xpub == meta.xpub && a.meta.network == meta.network
                }) {
                    self.say(true, format!("this account is already open as {}", self.accounts[i].meta.name));
                    self.select(i);
                    return;
                }
                match Account::create(meta, &self.wallets_dir()) {
                    Ok(a) => {
                        self.accounts.push(a);
                        let i = self.accounts.len() - 1;
                        self.tab = Tab::Transactions;
                        self.select(i);
                        self.say(true, "wallet added, scanning its history");
                    }
                    Err(e) => {
                        let mut n = n;
                        n.error = Some(e);
                        self.new_wallet = Some(n);
                    }
                }
            }
            (Purpose::Show { address }, Outcome::Address(shown)) => {
                if shown == address {
                    self.say(true, "the device shows the same address");
                } else {
                    self.say(false, format!("the device shows {shown}, which is not {address}. Do not use it."));
                }
            }
            (Purpose::Sign, Outcome::Signed(signed)) => self.add_signatures(signed),
            (Purpose::Message, Outcome::Message { address, signature }) => {
                let m = self.message.get_or_insert_with(MessageTool::default);
                m.address = address;
                m.signature = signature;
                m.result = Some((true, "signed, the signature is below".into()));
            }
            _ => {}
        }
    }

    pub fn add_signatures(&mut self, signed: Psbt) {
        let Some(i) = self.current else { return };
        let Some(r) = &mut self.review else { return };
        let a = &self.accounts[i];
        match a.merge(&mut r.psbt, signed) {
            Ok(()) => {
                r.error = None;
                match a.finalize(&r.psbt) {
                    Ok(tx) => r.tx = Some(tx),
                    Err(e) => r.error = Some(e),
                }
            }
            Err(e) => r.error = Some(e),
        }
        let msg = match (&r.tx, &r.error) {
            (Some(_), _) => (true, "signed, ready to broadcast".to_string()),
            (None, Some(e)) => (false, e.clone()),
            (None, None) => (true, "signatures added".to_string()),
        };
        self.say(msg.0, msg.1);
    }

    pub fn broadcast(&mut self) {
        let Some(i) = self.current else { return };
        let Some(tx) = self.review.as_ref().and_then(|r| r.tx.clone()) else { return };
        if self.pending.values().any(|p| matches!(p, Pending::Broadcast { .. })) {
            return;
        }
        let a = &self.accounts[i];
        let (servers, network, salt) = (a.meta.servers(), a.network(), a.meta.salt.as_bytes().to_vec());
        let hint = a.first_input_script(&tx);
        let file = a.file().to_path_buf();
        let label = self.review.as_ref().map(|r| r.built.label.clone()).unwrap_or_default();
        let id = self.id();
        self.pending.insert(id, Pending::Broadcast { file, label });
        self.hub.broadcast(id, servers, network, salt, tx, hint);
    }

    pub fn broadcasting(&self) -> bool {
        self.pending.values().any(|p| matches!(p, Pending::Broadcast { .. }))
    }

    pub fn fingerprint(&self) -> Option<Fingerprint> {
        self.account().and_then(|a| a.meta.fingerprint())
    }

    /// Open a PSBT someone else made or a Coldcard signed from its SD card.
    pub fn load_psbt(&mut self, bytes: &[u8]) {
        let Some(i) = self.current else {
            return self.say(false, "open a wallet before loading a transaction");
        };
        let psbt = match parse_psbt(bytes) {
            Ok(p) => p,
            Err(e) => return self.say(false, e),
        };
        // A signed copy of the transaction already being reviewed adds its signatures.
        if let Some(r) = &self.review
            && r.psbt.unsigned_tx.compute_txid() == psbt.unsigned_tx.compute_txid() {
                return self.add_signatures(psbt);
            }
        let a = &self.accounts[i];
        let vsize = psbt.unsigned_tx.vsize() as f64 + 27.0 * psbt.inputs.len() as f64;
        let rate = 0.0;
        match a.describe(psbt.clone(), rate, String::new()) {
            Ok(mut built) => {
                built.sat_per_vb = built.fee as f64 / vsize.max(1.0);
                let mut r = Review { psbt: psbt.clone(), built, tx: None, error: None, replaces: None };
                if let Ok(tx) = a.finalize(&psbt) {
                    r.tx = Some(tx);
                }
                self.review = Some(r);
                self.tab = Tab::Send;
            }
            Err(e) => self.say(false, e),
        }
    }

    pub fn fees_reset(&mut self) {
        self.fees_at = None;
    }

    fn dropped(&mut self, ctx: &egui::Context) {
        let files = ctx.input(|i| i.raw.dropped_files.clone());
        for f in files {
            let bytes = match (&f.bytes, &f.path) {
                (Some(b), _) => Some(b.to_vec()),
                (None, Some(p)) => std::fs::read(p).ok(),
                _ => None,
            };
            if let Some(b) = bytes {
                self.load_psbt(&b);
            }
        }
    }

    // ---- the shell ----

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("WALLET").font(legend_font(16.0)).color(VALUE).extra_letter_spacing(2.5));
            ui.add_space(18.0);
            if self.current.is_some() {
                let before = self.tab;
                ui.allocate_ui(egui::vec2(480.0, 24.0), |ui| tabs(
                    ui,
                    &mut self.tab,
                    &[
                        (Tab::Transactions, "transactions"),
                        (Tab::Send, "send"),
                        (Tab::Receive, "receive"),
                        (Tab::Addresses, "addresses"),
                        (Tab::Utxos, "utxos"),
                        (Tab::Settings, "settings"),
                    ],
                ));
                if before != self.tab {
                    self.label_edit.clear();
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let more = ui.add(Key::more().id_salt("menu")).on_hover_text("more");
                egui::Popup::menu(&more).width(220.0).show(|ui| {
                    let w = ui.available_width();
                    if ui.add(Key::new("sign or verify a message").outline(VALUE).min_width(w)).clicked() {
                        self.message = Some(MessageTool::default());
                    }
                    let open = self.current.is_some();
                    if ui.add(Key::new("load a psbt file").outline(VALUE).min_width(w).enabled(open)).clicked() {
                        self.browser = Some((
                            egui_bench::browser::Browser::new("load a transaction", home()).suffixes(&[".psbt", ".txt"]),
                            Browse::LoadPsbt,
                        ));
                    }
                    if ui.add(Key::new("paste a psbt").outline(VALUE).min_width(w).enabled(open)).clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                        self.say(true, "press ctrl+V to paste the PSBT text");
                    }
                    ui.add_space(4.0);
                    for u in Unit::ALL {
                        let label = format!("amounts in {}", u.label());
                        let tint = if self.unit == u { READOUT } else { VALUE };
                        if ui.add(Key::new(label).outline(tint).min_width(w)).clicked() {
                            self.unit = u;
                            self.save_config();
                        }
                    }
                });
                if let Some(i) = self.current {
                    let file = self.accounts[i].file().to_path_buf();
                    let busy = self.syncing(&file);
                    if ui
                        .add(Key::new(if busy { "syncing" } else { "sync" }).outline(TRACE).enabled(!busy))
                        .on_hover_text("ask the server for new transactions")
                        .clicked()
                    {
                        self.sync(i);
                    }
                }
                if ui.add(Key::new("add wallet").solid(READOUT)).clicked() {
                    self.new_wallet = Some(NewWallet::default());
                    self.devices_at = None;
                }
            });
        });
    }

    fn side(&mut self, ui: &mut egui::Ui) {
        heading(ui, "wallets");
        let mut pick = None;
        for (i, a) in self.accounts.iter().enumerate() {
            let on = self.current == Some(i);
            let rail = if on { READOUT } else { ETCH };
            let syncing = self.syncing(a.file());
            let r = card(
                ui,
                Some(rail),
                |ui| {
                    ui.horizontal(|ui| {
                        Line::new().legend(&a.meta.name).tint(if on { READOUT } else { VALUE }).elided(ui);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            chip(ui, a.meta.kind.label(), LEGEND, false);
                            if a.network() != Network::Bitcoin {
                                chip(ui, network_name(a.network()), WARN, true);
                            }
                        });
                    });
                },
                |ui| {
                    let b = a.balance();
                    Line::new().measured(self.unit.with_unit(b.total() as i64)).size(13.0).show(ui);
                    let when = match (syncing, a.meta.last_sync) {
                        (true, _) => "syncing".to_string(),
                        (false, Some(t)) => format!("synced {}", units::ago(t, units::now())),
                        (false, None) => "never synced".to_string(),
                    };
                    Line::new().note(format!("{} {}", a.meta.script.short(), a.meta.fingerprint)).gap(8.0).note(when).size(10.5).show(ui);
                },
            );
            if r.response.interact(egui::Sense::click()).clicked() && !on {
                pick = Some(i);
            }
        }
        if let Some(i) = pick {
            self.select(i);
        }
        for e in &self.load_errors {
            note(ui, format!("not loaded: {e}"), WARN);
        }
    }

    fn footer_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            match &self.notice {
                Some((ok, text, _)) => status(ui, *ok, text),
                None => match self.account() {
                    Some(a) => {
                        let syncing = self.pending.iter().find(|(_, p)| matches!(p, Pending::Sync(f) if f == a.file())).map(|(id, _)| *id);
                        let server = match syncing.and_then(|id| self.progress.get(&id)) {
                            Some(n) => format!("{}, {n} addresses checked", self.server(a)),
                            None => self.server(a),
                        };
                        let tip = a.tip();
                        Line::new().legend("server").value(server).gap(14.0).legend("height").measured(tip.to_string()).show(ui);
                    }
                    None => {
                        Line::new().note("no wallet open").show(ui);
                    }
                },
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(f) = &self.fees {
                    Line::new()
                        .legend("fees")
                        .measured(format!("{} / {} / {} sat/vB", rate(f.fast), rate(f.medium), rate(f.slow)))
                        .show(ui)
                        .on_hover_text("next block / 3 blocks / 6 blocks");
                }
            });
        });
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            ui.set_max_width(520.0);
            section(ui, "no wallets yet", "a watch-only wallet for a hardware signer", |ui| {
                note(
                    ui,
                    "Plug in a Jade, Coldcard or Trezor and add it. The wallet keeps only public keys; \
                     every payment is checked and signed on the device.",
                    LEGEND,
                );
                ui.add_space(6.0);
                if self.devices.is_empty() {
                    status(ui, false, "no device found yet, plug one in and unlock it");
                } else {
                    for d in &self.devices {
                        status(ui, true, &d.name);
                    }
                }
                ui.add_space(6.0);
                if ui.add(Key::new("add wallet").solid(READOUT).min_width(120.0)).clicked() {
                    self.new_wallet = Some(NewWallet::default());
                }
            });
        });
    }

    fn paste(&mut self, ctx: &egui::Context) {
        if self.current.is_none() || ctx.memory(|m| m.focused().is_some()) || self.device.is_some() {
            return;
        }
        let pasted: Vec<String> = ctx.input(|i| {
            i.events.iter().filter_map(|e| if let egui::Event::Paste(t) = e { Some(t.clone()) } else { None }).collect()
        });
        for t in pasted {
            if t.trim().starts_with("cHNidP") {
                self.load_psbt(t.as_bytes());
            }
        }
    }
}

/// `12`, `2.5`, `0.6`: whole above 10 sat/vB, one decimal below.
pub fn rate(r: f64) -> String {
    if r >= 10.0 { format!("{r:.0}") } else { format!("{r:.1}") }
}

pub fn parse_psbt(bytes: &[u8]) -> Result<Psbt, String> {
    if bytes.starts_with(b"psbt\xff") {
        return Psbt::deserialize(bytes).map_err(|e| format!("not a valid PSBT: {e}"));
    }
    let text = String::from_utf8_lossy(bytes);
    let t: String = text.split_whitespace().collect();
    t.parse::<Psbt>().map_err(|e| format!("not a valid PSBT: {e}"))
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

impl eframe::App for App {
    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        for a in &mut self.accounts {
            let _ = a.save();
        }
        self.save_config();
    }

    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        self.poll(&ctx);
        self.dropped(&ctx);
        self.paste(&ctx);
        self.refresh_views();
        if !self.pending.is_empty() || self.notice.is_some() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        egui::Panel::top("header")
            .frame(egui::Frame::NONE.fill(CHASSIS).inner_margin(Margin { left: 12, right: 12, top: 8, bottom: 6 }))
            .show(root, |ui| self.header(ui));
        egui::Panel::bottom("footer")
            .frame(egui::Frame::NONE.fill(CHASSIS).inner_margin(Margin { left: 12, right: 12, top: 5, bottom: 5 }))
            .show(root, |ui| self.footer_bar(ui));
        if !self.accounts.is_empty() {
            egui::Panel::left("side")
                .exact_size(SIDE_W)
                .resizable(false)
                .frame(egui::Frame::NONE.fill(CHASSIS).inner_margin(8))
                .show(root, |ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 8.0;
                        self.side(ui);
                    });
                });
        }
        egui::CentralPanel::no_frame().show(root, |ui| {
            let r = ui.max_rect();
            ui.painter().rect_filled(r, 0.0, CHASSIS);
            ui.painter().line_segment([r.left_top(), r.right_top()], Stroke::new(1.0, ETCH));
            egui::Frame::NONE.inner_margin(Margin::same(10)).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                if self.current.is_none() {
                    self.welcome(ui);
                } else {
                    match self.tab {
                        Tab::Transactions => self.transactions(ui),
                        Tab::Send => self.send_tab(ui),
                        Tab::Receive => self.receive_tab(ui),
                        Tab::Addresses => self.addresses_tab(ui),
                        Tab::Utxos => self.utxos_tab(ui),
                        Tab::Settings => self.settings_tab(ui),
                    }
                }
            });
        });

        self.new_wallet_modal(&ctx);
        self.device_modal(&ctx);
        self.message_modal(&ctx);
        self.remove_modal(&ctx);
        self.browser_modal(&ctx);
        if egui_bench::drop::hovering(&ctx).is_some() {
            egui_bench::drop::overlay(&ctx, None, "drop to open", "a PSBT to review or add signatures", READOUT);
        }
    }
}
