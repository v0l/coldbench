//! The watch-only wallet: descriptors from a device's xpub, kept in a file, with the views the
//! screens need and the steps of a payment (build, sign, finalise) as plain functions.

use crate::units;
use bdk_wallet::bitcoin::bip32::{DerivationPath, Fingerprint, Xpub};
use bdk_wallet::bitcoin::{
    self, Address, Amount, FeeRate, Network, OutPoint, Psbt, ScriptBuf, Sequence, Transaction,
    Txid,
};
use bdk_wallet::chain::Merge;
use bdk_wallet::chain::spk_client::{FullScanRequest, FullScanResponse};
use bdk_wallet::{ChangeSet, KeychainKind, Wallet};
use hww::{Kind, Script};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// How many unused addresses past the last used one are watched, same as Sparrow and Core.
pub const GAP: usize = 20;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    pub name: String,
    pub network: Network,
    pub script: Script,
    pub kind: Kind,
    /// Master fingerprint of the device, as hex.
    pub fingerprint: String,
    pub account: u32,
    pub xpub: String,
    /// The wallet's own servers, one per line (`https://host/api` or `ssl://host:port`). Empty
    /// spreads the scan over the public servers. An older file's single Esplora url lands here.
    #[serde(default)]
    pub server: String,
    /// Mixed into how addresses are dealt to servers, so two wallets split differently and an
    /// operator cannot guess which slice it holds.
    #[serde(default = "new_salt")]
    pub salt: String,
    /// Requests in flight at once across all servers.
    #[serde(default = "default_parallel")]
    pub parallel: usize,
    pub created: u64,
    #[serde(default)]
    pub last_sync: Option<u64>,
    /// Notes the person wrote, keyed by txid, address or `txid:vout`.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// Transactions held, those anchored at a height, and the last revealed index per keychain.
pub type Known = (std::collections::HashSet<Txid>, std::collections::HashSet<(Txid, u32)>, BTreeMap<KeychainKind, u32>);

pub fn new_salt() -> String {
    use bdk_wallet::bitcoin::secp256k1::rand::{RngCore, thread_rng};
    let mut b = [0u8; 16];
    thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn default_parallel() -> usize {
    8
}

impl Meta {
    /// The servers to sync from: the wallet's own, or every public one for its network.
    pub fn servers(&self) -> Vec<crate::net::servers::Server> {
        let own: Vec<_> = self.server.lines().filter_map(crate::net::servers::Server::parse).collect();
        if own.is_empty() { crate::net::servers::public(self.network) } else { own }
    }

    pub fn spread(&self) -> bool {
        self.server.lines().all(|l| l.trim().is_empty())
    }

    pub fn fingerprint(&self) -> Option<Fingerprint> {
        Fingerprint::from_str(&self.fingerprint).ok()
    }

    pub fn account_path(&self) -> DerivationPath {
        hww::account_path(self.script, self.network, self.account)
    }
}

#[derive(Serialize, Deserialize)]
struct File {
    meta: Meta,
    changeset: ChangeSet,
}

pub struct Account {
    pub meta: Meta,
    wallet: Wallet,
    saved: ChangeSet,
    path: PathBuf,
    /// Bumped whenever what the screens show may have changed, so they can cache.
    pub revision: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Balance {
    pub confirmed: u64,
    pub pending: u64,
    pub immature: u64,
}

impl Balance {
    pub fn total(&self) -> u64 {
        self.confirmed + self.pending + self.immature
    }
}

#[derive(Clone, Debug)]
pub struct TxRow {
    pub txid: Txid,
    pub height: Option<u32>,
    pub time: Option<u64>,
    pub confirmations: u32,
    pub delta: i64,
    pub balance: i64,
    pub fee: Option<u64>,
    pub label: String,
    pub tx: Transaction,
}

#[derive(Clone, Debug)]
pub struct AddrRow {
    pub keychain: KeychainKind,
    pub index: u32,
    pub address: String,
    pub path: String,
    pub label: String,
    pub balance: u64,
    pub received: usize,
}

#[derive(Clone, Debug)]
pub struct UtxoRow {
    pub outpoint: OutPoint,
    pub address: String,
    pub value: u64,
    pub label: String,
    pub confirmations: u32,
    pub frozen: bool,
    pub change: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SendPlan {
    pub recipients: Vec<(Address, u64)>,
    /// The last recipient gets whatever is left after the others and the fee.
    pub max: bool,
    pub sat_per_vb: f64,
    pub rbf: bool,
    /// Spend exactly these and nothing else. `None` lets the wallet choose.
    pub only: Option<Vec<OutPoint>>,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct Output {
    pub address: String,
    pub value: u64,
    pub change: bool,
}

#[derive(Clone, Debug)]
pub struct Input {
    pub outpoint: OutPoint,
    pub address: String,
    pub value: u64,
}

#[derive(Clone, Debug)]
pub struct Built {
    pub psbt: Psbt,
    pub inputs: Vec<Input>,
    pub outputs: Vec<Output>,
    pub fee: u64,
    pub sat_per_vb: f64,
    pub label: String,
}

impl Account {
    // ---- files ----

    pub fn create(meta: Meta, dir: &Path) -> Result<Account, String> {
        let xpub = parse_xpub(&meta.xpub, meta.network)?;
        let fp = meta.fingerprint().ok_or("the device fingerprint is not valid")?;
        let (external, internal) = descriptors(meta.script, fp, &meta.account_path(), &xpub);
        let mut wallet = Wallet::create(external, internal)
            .network(meta.network)
            .lookahead(GAP as u32)
            .create_wallet_no_persist()
            .map_err(|e| format!("cannot make a wallet from that key: {e}"))?;
        wallet.reveal_next_address(KeychainKind::External);
        let path = dir.join(file_name(&meta));
        let mut a = Account {
            meta,
            saved: ChangeSet::default(),
            wallet,
            path,
            revision: 1,
        };
        a.save()?;
        Ok(a)
    }

    pub fn load(path: &Path) -> Result<Account, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file: File =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let meta = file.meta;
        let xpub = parse_xpub(&meta.xpub, meta.network)?;
        let fp = meta.fingerprint().ok_or("the device fingerprint is not valid")?;
        let (external, internal) = descriptors(meta.script, fp, &meta.account_path(), &xpub);
        let wallet = Wallet::load()
            .descriptor(KeychainKind::External, Some(external))
            .descriptor(KeychainKind::Internal, Some(internal))
            .check_network(meta.network)
            .lookahead(GAP as u32)
            .load_wallet_no_persist(file.changeset.clone())
            .map_err(|e| format!("{}: {e}", path.display()))?
            .ok_or_else(|| format!("{}: holds no wallet", path.display()))?;
        let fresh_salt = !text.contains("\"salt\"");
        let mut a = Account { meta, wallet, saved: file.changeset, path: path.to_path_buf(), revision: 1 };
        // A file from before the salt existed gets one now, and keeps it: a salt that changed
        // on every start would deal the addresses to new servers each time.
        if fresh_salt {
            a.save()?;
        }
        Ok(a)
    }

    pub fn file(&self) -> &Path {
        &self.path
    }

    /// Write everything staged, and the labels, to disk. Writes beside the file and renames, so a
    /// crash leaves the old copy.
    pub fn save(&mut self) -> Result<(), String> {
        if let Some(staged) = self.wallet.take_staged() {
            self.saved.merge(staged);
        }
        let file = File { meta: self.meta.clone(), changeset: self.saved.clone() };
        let text = serde_json::to_string(&file).map_err(|e| e.to_string())?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("{}: {e}", self.path.display()))?;
        self.revision += 1;
        Ok(())
    }

    // ---- reading ----

    pub fn network(&self) -> Network {
        self.meta.network
    }

    pub fn tip(&self) -> u32 {
        self.wallet.latest_checkpoint().height()
    }

    pub fn balance(&self) -> Balance {
        let b = self.wallet.balance();
        Balance {
            confirmed: b.confirmed.to_sat(),
            pending: (b.trusted_pending + b.untrusted_pending).to_sat(),
            immature: b.immature.to_sat(),
        }
    }

    pub fn full_path(&self, keychain: KeychainKind, index: u32) -> DerivationPath {
        let chain = if keychain == KeychainKind::External { 0 } else { 1 };
        format!("{}/{chain}/{index}", self.meta.account_path())
            .parse()
            .expect("account path plus two indexes")
    }

    pub fn label(&self, key: &str) -> &str {
        self.meta.labels.get(key).map(String::as_str).unwrap_or("")
    }

    pub fn set_label(&mut self, key: &str, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            self.meta.labels.remove(key);
        } else {
            self.meta.labels.insert(key.to_string(), text.to_string());
        }
        self.revision += 1;
    }

    pub fn txs(&self) -> Vec<TxRow> {
        use bdk_wallet::chain::ChainPosition;
        let tip = self.tip();
        let mut rows: Vec<TxRow> = self
            .wallet
            .transactions()
            .map(|t| {
                let (sent, received) = self.wallet.sent_and_received(&t.tx_node.tx);
                let txid = t.tx_node.txid;
                let (height, time) = match &t.chain_position {
                    ChainPosition::Confirmed { anchor, .. } => {
                        (Some(anchor.block_id.height), Some(anchor.confirmation_time))
                    }
                    ChainPosition::Unconfirmed { first_seen, last_seen } => {
                        (None, first_seen.or(*last_seen))
                    }
                };
                TxRow {
                    txid,
                    height,
                    time,
                    confirmations: height.map(|h| tip.saturating_sub(h) + 1).unwrap_or(0),
                    delta: received.to_sat() as i64 - sent.to_sat() as i64,
                    balance: 0,
                    fee: self.wallet.calculate_fee(&t.tx_node.tx).ok().map(|f| f.to_sat()),
                    label: self.label(&txid.to_string()).to_string(),
                    tx: (*t.tx_node.tx).clone(),
                }
            })
            .collect();
        // Oldest first to run the balance, then newest first to show.
        rows.sort_by_key(|r| (r.height.unwrap_or(u32::MAX), r.time.unwrap_or(u64::MAX)));
        order_parents_first(&mut rows);
        let mut running = 0;
        for r in &mut rows {
            running += r.delta;
            r.balance = running;
        }
        rows.reverse();
        rows
    }

    pub fn utxos(&self) -> Vec<UtxoRow> {
        let tip = self.tip();
        let mut rows: Vec<UtxoRow> = self
            .wallet
            .list_unspent()
            .map(|u| {
                let key = u.outpoint.to_string();
                let height = match &u.chain_position {
                    bdk_wallet::chain::ChainPosition::Confirmed { anchor, .. } => {
                        Some(anchor.block_id.height)
                    }
                    _ => None,
                };
                UtxoRow {
                    outpoint: u.outpoint,
                    address: self.address_of(&u.txout.script_pubkey),
                    value: u.txout.value.to_sat(),
                    // A coin without its own label takes the label of the payment that made it.
                    label: Some(self.label(&key))
                        .filter(|l| !l.is_empty())
                        .unwrap_or_else(|| self.label(&u.outpoint.txid.to_string()))
                        .to_string(),
                    confirmations: height.map(|h| tip.saturating_sub(h) + 1).unwrap_or(0),
                    frozen: self.wallet.is_outpoint_locked(u.outpoint),
                    change: u.keychain == KeychainKind::Internal,
                }
            })
            .collect();
        rows.sort_by(|a, b| b.value.cmp(&a.value).then(a.outpoint.cmp(&b.outpoint)));
        rows
    }

    pub fn addresses(&self, keychain: KeychainKind) -> Vec<AddrRow> {
        let mut balance: HashMap<ScriptBuf, u64> = HashMap::new();
        let mut received: HashMap<ScriptBuf, usize> = HashMap::new();
        for o in self.wallet.list_output() {
            *received.entry(o.txout.script_pubkey.clone()).or_default() += 1;
            if !o.is_spent {
                *balance.entry(o.txout.script_pubkey.clone()).or_default() += o.txout.value.to_sat();
            }
        }
        self.wallet
            .spk_index()
            .revealed_keychain_spks(keychain)
            .map(|(index, spk)| {
                let address = self.address_of(&spk);
                AddrRow {
                    keychain,
                    index,
                    path: self.full_path(keychain, index).to_string(),
                    label: self.label(&address).to_string(),
                    balance: balance.get(&spk).copied().unwrap_or(0),
                    received: received.get(&spk).copied().unwrap_or(0),
                    address,
                }
            })
            .collect()
    }

    /// The outputs of `tx` as address, value, and whether this wallet owns it.
    pub fn outputs_of(&self, tx: &Transaction) -> Vec<(String, u64, bool)> {
        tx.output
            .iter()
            .map(|o| {
                let mine = self.wallet.is_mine(o.script_pubkey.clone());
                (self.address_of(&o.script_pubkey), o.value.to_sat(), mine)
            })
            .collect()
    }

    fn address_of(&self, spk: &ScriptBuf) -> String {
        Address::from_script(spk, self.meta.network)
            .map(|a| a.to_string())
            .unwrap_or_else(|_| spk.to_hex_string())
    }

    pub fn is_mine(&self, address: &str) -> bool {
        Address::from_str(address)
            .ok()
            .and_then(|a| a.require_network(self.meta.network).ok())
            .is_some_and(|a| self.wallet.is_mine(a.script_pubkey()))
    }

    /// Where `address` sits in this wallet, if it is one of ours.
    pub fn locate(&self, address: &str) -> Option<(KeychainKind, u32)> {
        let a = Address::from_str(address).ok()?.require_network(self.meta.network).ok()?;
        self.wallet.derivation_of_spk(a.script_pubkey())
    }

    pub fn descriptor(&self, keychain: KeychainKind) -> String {
        self.wallet.public_descriptor(keychain).to_string()
    }

    // ---- receiving ----

    /// The address to give out now: the first one nobody has paid.
    pub fn receive(&mut self) -> AddrRow {
        let info = self.wallet.next_unused_address(KeychainKind::External);
        self.revision += 1;
        self.row_for(info.keychain, info.index)
    }

    /// A fresh address even if the last one is unused.
    pub fn another(&mut self) -> AddrRow {
        let info = self.wallet.reveal_next_address(KeychainKind::External);
        self.revision += 1;
        self.row_for(info.keychain, info.index)
    }

    fn row_for(&self, keychain: KeychainKind, index: u32) -> AddrRow {
        self.addresses(keychain)
            .into_iter()
            .find(|r| r.index == index)
            .expect("an address just revealed is listed")
    }

    // ---- syncing ----

    pub fn scan_request(&self) -> FullScanRequest<KeychainKind> {
        self.wallet.start_full_scan().build()
    }

    /// What the scanner can skip fetching: transactions it has, and those already anchored.
    pub fn known(&self) -> Known {
        use bdk_wallet::chain::ChainPosition;
        let mut have = std::collections::HashSet::new();
        let mut anchored = std::collections::HashSet::new();
        for t in self.wallet.transactions() {
            have.insert(t.tx_node.txid);
            if let ChainPosition::Confirmed { anchor, .. } = &t.chain_position {
                anchored.insert((t.tx_node.txid, anchor.block_id.height));
            }
        }
        let floor = [KeychainKind::External, KeychainKind::Internal]
            .into_iter()
            .filter_map(|k| Some((k, self.wallet.spk_index().last_revealed_index(k)?)))
            .collect();
        (have, anchored, floor)
    }

    /// The script of the first input of `tx` that is ours: where a broadcast goes so no new
    /// server learns of the payment.
    pub fn first_input_script(&self, tx: &Transaction) -> ScriptBuf {
        tx.input
            .iter()
            .find_map(|i| self.wallet.tx_graph().get_txout(i.previous_output).map(|o| o.script_pubkey.clone()))
            .or_else(|| tx.output.first().map(|o| o.script_pubkey.clone()))
            .unwrap_or_default()
    }

    pub fn apply_scan(&mut self, update: FullScanResponse<KeychainKind>) -> Result<(), String> {
        self.wallet.apply_update(update).map_err(|e| format!("the server's answer does not fit the chain we have: {e}"))?;
        self.meta.last_sync = Some(units::now());
        self.save()
    }

    pub fn lock(&mut self, outpoint: OutPoint, frozen: bool) {
        if frozen {
            self.wallet.lock_outpoint(outpoint);
        } else {
            self.wallet.unlock_outpoint(outpoint);
        }
        self.revision += 1;
    }

    // ---- paying ----

    pub fn parse_address(&self, text: &str) -> Result<Address, String> {
        let a = Address::from_str(text.trim()).map_err(|_| "that is not a bitcoin address".to_string())?;
        a.require_network(self.meta.network).map_err(|_| {
            format!("that address is for another network, this wallet is on {}", network_name(self.meta.network))
        })
    }

    pub fn build(&mut self, plan: &SendPlan) -> Result<Built, String> {
        if plan.recipients.is_empty() {
            return Err("add somewhere to send to".into());
        }
        if !(plan.sat_per_vb.is_finite() && plan.sat_per_vb >= crate::net::MIN_RATE) {
            return Err("the fee rate must be at least 0.1 sat/vB".into());
        }
        let rate = FeeRate::from_sat_per_kwu((plan.sat_per_vb * 250.0).round() as u64);
        let mut b = self.wallet.build_tx();
        b.fee_rate(rate);
        if !plan.rbf {
            b.set_exact_sequence(Sequence::ENABLE_LOCKTIME_NO_RBF);
        }
        if let Some(only) = &plan.only {
            b.add_utxos(only).map_err(|e| format!("cannot spend the chosen coins: {e}"))?;
            b.manually_selected_only();
        }
        let last = plan.recipients.len() - 1;
        for (i, (addr, value)) in plan.recipients.iter().enumerate() {
            if plan.max && i == last {
                if plan.only.is_none() {
                    b.drain_wallet();
                }
                b.drain_to(addr.script_pubkey());
            } else {
                if *value == 0 {
                    return Err("an amount is zero".into());
                }
                b.add_recipient(addr.script_pubkey(), Amount::from_sat(*value));
            }
        }
        let psbt = b.finish().map_err(explain_build_error)?;
        self.revision += 1;
        self.describe(psbt, plan.sat_per_vb, plan.label.clone())
    }

    /// Can `row` still be replaced with a higher fee? Ours, unconfirmed and signalling RBF.
    pub fn can_bump(row: &TxRow) -> bool {
        row.height.is_none() && row.delta < 0 && row.tx.is_explicitly_rbf()
    }

    /// A replacement for an unconfirmed payment of ours that pays `sat_per_vb`.
    pub fn bump(&mut self, txid: Txid, sat_per_vb: f64) -> Result<Built, String> {
        if !(sat_per_vb.is_finite() && sat_per_vb >= crate::net::MIN_RATE) {
            return Err("the fee rate must be at least 0.1 sat/vB".into());
        }
        let rate = FeeRate::from_sat_per_kwu((sat_per_vb * 250.0).round() as u64);
        let mut b = self
            .wallet
            .build_fee_bump(txid)
            .map_err(|e| format!("cannot bump this transaction: {e}"))?;
        b.fee_rate(rate);
        let psbt = b.finish().map_err(explain_build_error)?;
        self.revision += 1;
        let label = self.label(&txid.to_string()).to_string();
        self.describe(psbt, sat_per_vb, label)
    }

    /// Record a transaction we just broadcast, so it shows before the next sync.
    pub fn add_sent(&mut self, tx: &Transaction, label: &str) -> Result<(), String> {
        let txid = tx.compute_txid();
        self.wallet.apply_unconfirmed_txs([(tx.clone(), units::now())]);
        if !label.trim().is_empty() {
            self.set_label(&txid.to_string(), label);
        }
        self.save()
    }

    /// Read a PSBT back into what a person checks before signing.
    pub fn describe(&self, psbt: Psbt, sat_per_vb: f64, label: String) -> Result<Built, String> {
        let tx = &psbt.unsigned_tx;
        let mut inputs = Vec::new();
        let mut in_total = 0u64;
        for (i, txin) in tx.input.iter().enumerate() {
            let utxo = bdk_wallet::PsbtUtils::get_utxo_for(&psbt, i)
                .ok_or_else(|| format!("input {i} does not say how much it holds"))?;
            in_total += utxo.value.to_sat();
            inputs.push(Input {
                outpoint: txin.previous_output,
                address: self.address_of(&utxo.script_pubkey),
                value: utxo.value.to_sat(),
            });
        }
        let outputs: Vec<Output> = tx
            .output
            .iter()
            .map(|o| Output {
                address: self.address_of(&o.script_pubkey),
                value: o.value.to_sat(),
                change: self
                    .wallet
                    .derivation_of_spk(o.script_pubkey.clone())
                    .is_some_and(|(k, _)| k == KeychainKind::Internal),
            })
            .collect();
        let out_total: u64 = outputs.iter().map(|o| o.value).sum();
        let fee = in_total.checked_sub(out_total).ok_or("the outputs add up to more than the inputs")?;
        Ok(Built { psbt, inputs, outputs, fee, sat_per_vb, label })
    }

    /// Put the signatures a device gave back into our copy.
    pub fn merge(&self, mine: &mut Psbt, signed: Psbt) -> Result<(), String> {
        if signed.unsigned_tx.compute_txid() != mine.unsigned_tx.compute_txid() {
            return Err("the device signed a different transaction than the one shown".into());
        }
        mine.combine(signed).map_err(|e| format!("cannot combine the signatures: {e}"))
    }

    /// How many inputs have a signature or are final.
    pub fn signed_inputs(psbt: &Psbt) -> usize {
        psbt.inputs
            .iter()
            .filter(|i| {
                !i.partial_sigs.is_empty()
                    || i.final_script_witness.is_some()
                    || i.final_script_sig.is_some()
            })
            .count()
    }

    /// Turn a fully signed PSBT into the transaction to broadcast.
    pub fn finalize(&self, psbt: &Psbt) -> Result<Transaction, String> {
        let mut p = psbt.clone();
        let outcome = self.wallet.try_finalize_psbt(&mut p).map_err(|e| e.to_string())?;
        if !outcome.is_finalized() {
            let missing = Self::signed_inputs(&p);
            return Err(format!(
                "{missing} of {} inputs are signed, the rest still need a signature",
                p.inputs.len()
            ));
        }
        p.extract_tx().map_err(|e| format!("the signed transaction is not valid: {e}"))
    }

    /// Check a message signature, BIP137 style (base64, header byte says the address type).
    pub fn verify_message(&self, address: &str, message: &str, signature: &str) -> Result<bool, String> {
        verify_message(address, message, signature, self.meta.network)
    }
}

/// Within a block (or the mempool) a transaction may spend another one beside it, and they share a
/// time. Move every transaction after the ones it spends so the running balance never dips.
fn order_parents_first(rows: &mut Vec<TxRow>) {
    let mut out: Vec<TxRow> = Vec::with_capacity(rows.len());
    let mut placed = std::collections::HashSet::new();
    let mut rest: Vec<TxRow> = std::mem::take(rows);
    while !rest.is_empty() {
        let height = rest[0].height;
        let group: Vec<Txid> = rest.iter().take_while(|r| r.height == height).map(|r| r.txid).collect();
        let ready = rest.iter().position(|r| {
            r.height == height
                && r.tx.input.iter().all(|i| {
                    placed.contains(&i.previous_output.txid) || !group.contains(&i.previous_output.txid)
                })
        });
        let row = rest.remove(ready.unwrap_or(0));
        placed.insert(row.txid);
        out.push(row);
    }
    *rows = out;
}

pub fn network_name(n: Network) -> &'static str {
    match n {
        Network::Bitcoin => "mainnet",
        Network::Testnet => "testnet",
        Network::Testnet4 => "testnet4",
        Network::Signet => "signet",
        Network::Regtest => "regtest",
    }
}

fn explain_build_error(e: bdk_wallet::error::CreateTxError) -> String {
    use bdk_wallet::error::CreateTxError as E;
    match e {
        E::CoinSelection(c) => format!("not enough funds: needs {} sats more", c.needed.to_sat().saturating_sub(c.available.to_sat())),
        E::OutputBelowDustLimit(i) => format!("output {} is below the dust limit, send at least 546 sats", i + 1),
        E::NoRecipients => "add somewhere to send to".into(),
        E::NoUtxosSelected => "no coins are selected".into(),
        E::FeeRateTooLow { required } => format!("the fee rate is too low, the minimum is {} sat/vB", required.to_sat_per_vb_ceil()),
        other => other.to_string(),
    }
}

fn parse_xpub(text: &str, network: Network) -> Result<Xpub, String> {
    let mut x = Xpub::from_str(text.trim()).map_err(|e| format!("the device's key is not valid: {e}"))?;
    // Devices differ on whether a test network key is written xpub or tpub; descriptors need the
    // one that matches the wallet.
    x.network = network.into();
    Ok(x)
}

fn descriptors(script: Script, fp: Fingerprint, path: &DerivationPath, xpub: &Xpub) -> (String, String) {
    let origin = format!("[{fp}/{path}]");
    let key = |chain: u32| format!("{origin}{xpub}/{chain}/*");
    (script.descriptor(&key(0)), script.descriptor(&key(1)))
}

fn file_name(meta: &Meta) -> String {
    let clean: String = meta
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    format!("{}-{}-{}.json", clean.trim_matches('-'), meta.fingerprint, meta.created)
}

/// Does `signature` (base64) sign `message` for `address`?
pub fn verify_message(address: &str, message: &str, signature: &str, network: Network) -> Result<bool, String> {
    use bdk_wallet::bitcoin::base64::{Engine, engine::general_purpose::STANDARD};
    use bdk_wallet::bitcoin::secp256k1::{Message, Secp256k1, ecdsa::{RecoverableSignature, RecoveryId}};
    use bdk_wallet::bitcoin::{CompressedPublicKey, sign_message::signed_msg_hash};

    let address = Address::from_str(address.trim())
        .map_err(|_| "that is not a bitcoin address")?
        .require_network(network)
        .map_err(|_| "that address is for another network")?;
    let raw = STANDARD.decode(signature.trim()).map_err(|_| "the signature is not base64")?;
    if raw.len() != 65 {
        return Err("a signature is 65 bytes".into());
    }
    let header = raw[0];
    if !(27..=42).contains(&header) {
        return Err("the first byte of the signature is not a message signature header".into());
    }
    let id = RecoveryId::from_i32(((header - 27) % 4) as i32).map_err(|e| e.to_string())?;
    let sig = RecoverableSignature::from_compact(&raw[1..], id).map_err(|e| e.to_string())?;
    let hash = signed_msg_hash(message);
    let secp = Secp256k1::new();
    let key = secp
        .recover_ecdsa(&Message::from_digest(*hash.as_ref()), &sig)
        .map_err(|e| e.to_string())?;
    let compressed = CompressedPublicKey(key);
    let spk = address.script_pubkey();
    let candidates = [
        Address::p2pkh(compressed, network).script_pubkey(),
        Address::p2shwpkh(&compressed, network).script_pubkey(),
        Address::p2wpkh(&compressed, network).script_pubkey(),
        Address::p2pkh(bitcoin::PublicKey::new_uncompressed(key), network).script_pubkey(),
    ];
    // Some signers use the p2pkh header for every address type, so accept any form of the same key.
    Ok(candidates.contains(&spk))
}

#[cfg(test)]
mod tests;
