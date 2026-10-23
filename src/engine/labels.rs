//! Wallet labels in BIP329's format, the one Sparrow writes from File, Export Wallet, Labels and
//! reads back the same way: JSON Lines, one record per label.
//!
//! ```text
//! {"type":"tx","ref":"f91d...aafd","label":"rent","origin":"wpkh([d34db33f/84'/0'/0'])"}
//! {"type":"addr","ref":"bc1q34aq...","label":"from Alice"}
//! {"type":"output","ref":"f91d...aafd:1","label":"change","spendable":false}
//! ```
//!
//! The wallet's own label store already keys on a txid, an address or `txid:vout`, so tx, addr
//! and output records map straight onto it. An output with `"spendable": false` is a frozen coin.
//! Records about other wallets (by `origin`, or by refs this wallet does not know) are kept out
//! and counted, as are the types the wallet has no use for (pubkey, xpub, input, spscan).

use super::Account;
use bdk_wallet::bitcoin::{OutPoint, Txid};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// BIP329 suggests importers may cut labels to 255 characters.
const MAX_LABEL: usize = 255;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Record {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spendable: Option<bool>,
}

/// What an import did, for one line in the window.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Imported {
    pub labels: usize,
    /// Labels that replaced a different one already here.
    pub replaced: usize,
    pub frozen: usize,
    pub unfrozen: usize,
    /// For a txid, address or coin this wallet has never seen.
    pub unknown: usize,
    /// From another account, by `origin`.
    pub other_wallet: usize,
    /// pubkey, xpub, input, spscan: kinds this wallet keeps no labels for.
    pub skipped: usize,
    pub cut: usize,
    /// Lines that are not BIP329, with their line numbers.
    pub bad: Vec<usize>,
}

impl Imported {
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("{} labels imported", self.labels)];
        if self.replaced > 0 {
            parts.push(format!("{} replaced", self.replaced));
        }
        if self.frozen + self.unfrozen > 0 {
            parts.push(format!("{} coins frozen, {} unfrozen", self.frozen, self.unfrozen));
        }
        if self.unknown > 0 {
            parts.push(format!("{} for things this wallet has not seen (sync first?)", self.unknown));
        }
        if self.other_wallet > 0 {
            parts.push(format!("{} from another account", self.other_wallet));
        }
        if self.skipped > 0 {
            parts.push(format!("{} of kinds kept nowhere here", self.skipped));
        }
        if self.cut > 0 {
            parts.push(format!("{} cut to {MAX_LABEL} characters", self.cut));
        }
        if !self.bad.is_empty() {
            let lines: Vec<String> = self.bad.iter().take(5).map(|n| n.to_string()).collect();
            parts.push(format!("{} lines not understood ({})", self.bad.len(), lines.join(", ")));
        }
        parts.join(", ")
    }
}

/// Read a BIP329 file: one JSON object per line. Blank lines are skipped, bad lines counted.
pub fn parse(text: &str) -> (Vec<Record>, Vec<usize>) {
    let mut records = Vec::new();
    let mut bad = Vec::new();
    // Some writers add a byte order mark.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Record>(line) {
            Ok(r) => records.push(r),
            Err(_) => bad.push(n + 1),
        }
    }
    (records, bad)
}

/// The key-origin part of an `origin`, lower case and with `h` for hardened, so
/// `wpkh([D34DB33F/84h/0h/0h])` and `wpkh([d34db33f/84'/0'/0'])` compare equal.
fn origin_key(origin: &str) -> Option<String> {
    let start = origin.find('[')? + 1;
    let end = origin[start..].find(']')? + start;
    Some(origin[start..end].to_lowercase().replace('\'', "h"))
}

impl Account {
    /// The origin this wallet writes on its records: `wpkh([fp/84'/0'/0'])`.
    pub fn origin(&self) -> String {
        // `DerivationPath` prints hardened steps with `'`, as Sparrow writes them.
        let key = format!("[{}/{}]", self.meta.fingerprint, self.meta.account_path());
        self.meta.script.descriptor(&key)
    }

    /// Bring in labels and frozen coins. A label for something this wallet already has replaces
    /// it; nothing is ever removed by an import.
    pub fn import_labels(&mut self, text: &str) -> Imported {
        let (records, bad) = parse(text);
        let mine = origin_key(&self.origin());
        let have_tx: std::collections::HashSet<Txid> = self.wallet.transactions().map(|t| t.tx_node.txid).collect();
        let mut out = Imported { bad, ..Default::default() };
        for r in records {
            if let Some(o) = &r.origin
                && origin_key(o).is_some() && origin_key(o) != mine {
                    out.other_wallet += 1;
                    continue;
                }
            let reference = r.reference.trim();
            let known = match r.kind.as_str() {
                "tx" => Txid::from_str(reference).ok().is_some_and(|t| have_tx.contains(&t)),
                "addr" => self.is_mine(reference),
                "output" => OutPoint::from_str(reference).ok().is_some_and(|o| self.wallet.tx_graph().get_txout(o).is_some_and(|t| self.wallet.is_mine(t.script_pubkey.clone()))),
                _ => {
                    out.skipped += 1;
                    continue;
                }
            };
            if !known {
                out.unknown += 1;
                continue;
            }
            // Store addresses as the wallet prints them, so a label typed in upper case bech32
            // still finds its address.
            let key = match r.kind.as_str() {
                "addr" => self.canonical_address(reference).unwrap_or_else(|| reference.to_string()),
                _ => reference.to_lowercase(),
            };
            if let Some(label) = r.label.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
                let label: String = if label.chars().count() > MAX_LABEL {
                    out.cut += 1;
                    label.chars().take(MAX_LABEL).collect()
                } else {
                    label.to_string()
                };
                let before = self.label(&key).to_string();
                if before != label {
                    if !before.is_empty() {
                        out.replaced += 1;
                    }
                    self.set_label(&key, &label);
                }
                out.labels += 1;
            }
            if r.kind == "output"
                && let (Some(spendable), Ok(o)) = (r.spendable, OutPoint::from_str(&key)) {
                    let frozen = self.wallet.is_outpoint_locked(o);
                    if spendable == frozen {
                        self.lock(o, !spendable);
                        if spendable { out.unfrozen += 1 } else { out.frozen += 1 }
                    }
                }
        }
        out
    }

    /// Every label and frozen coin as BIP329, for Sparrow or any wallet that reads it.
    pub fn export_labels(&self) -> String {
        let origin = self.origin();
        let mut lines = Vec::new();
        let mut frozen: std::collections::BTreeSet<String> =
            self.wallet.list_locked_outpoints().map(|o| o.to_string()).collect();
        for (key, label) in &self.meta.labels {
            let kind = if Txid::from_str(key).is_ok() {
                "tx"
            } else if OutPoint::from_str(key).is_ok() {
                "output"
            } else {
                "addr"
            };
            let spendable = (kind == "output").then(|| !frozen.remove(key));
            let r = Record {
                kind: kind.into(),
                reference: key.clone(),
                label: Some(label.clone()),
                origin: (kind == "tx").then(|| origin.clone()),
                spendable,
            };
            lines.push(serde_json::to_string(&r).expect("a record serialises"));
        }
        for key in frozen {
            let r = Record { kind: "output".into(), reference: key, label: None, origin: None, spendable: Some(false) };
            lines.push(serde_json::to_string(&r).expect("a record serialises"));
        }
        let mut s = lines.join("\n");
        s.push('\n');
        s
    }

    fn canonical_address(&self, text: &str) -> Option<String> {
        self.parse_address(text).ok().map(|a| a.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test vectors from BIP329, verbatim.
    const VECTORS: &str = r#"{ "type": "tx", "ref": "f91d0a8a78462bc59398f2c5d7a84fcff491c26ba54c4833478b202796c8aafd", "label": "Transaction", "origin": "wpkh([d34db33f/84'/0'/0'])" }
{ "type": "addr", "ref": "bc1q34aq5drpuwy3wgl9lhup9892qp6svr8ldzyy7c", "label": "Address" }
{ "type": "pubkey", "ref": "0283409659355b6d1cc3c32decd5d561abaac86c37a353b52895a5e6c196d6f448", "label": "Public Key" }
{ "type": "input", "ref": "f91d0a8a78462bc59398f2c5d7a84fcff491c26ba54c4833478b202796c8aafd:0", "label": "Input" }
{ "type": "output", "ref": "f91d0a8a78462bc59398f2c5d7a84fcff491c26ba54c4833478b202796c8aafd:1", "label": "Output", "spendable": false }
{ "type": "xpub", "ref": "xpub661MyMwAqRbcFtXgS5sYJABqqG9YLmC4Q1Rdap9gSE8NqtwybGhePY2gZ29ESFjqJoCu1Rupje8YtGqsefD265TMg7usUDFdp6W1EGMcet8", "label": "Extended Public Key" }
{ "type": "spscan", "ref": "spscan1q5zs2pg9q5zs2pg9q5zs2pg9q5zs2pg9q5zs2pg9q5zs2pg9q5zsq9q6qjevn2kmdrnpuxt0v6h2kr2a2epkr0g6nk55ftf0xcxtddazgkrth3e", "label": "Silent Payments Scan Key Expression" }
{ "type": "tx", "ref": "f546156d9044844e02b181026a1a407abfca62e7ea1159f87bbeaa77b4286c74", "label": "Account #1 Transaction", "origin": "wpkh([d34db33f/84'/0'/1'])" }
"#;

    #[test]
    fn the_bip_vectors_parse() {
        let (r, bad) = parse(VECTORS);
        assert!(bad.is_empty());
        assert_eq!(r.len(), 8);
        assert_eq!(r[4].spendable, Some(false));
        assert_eq!(r[0].origin.as_deref(), Some("wpkh([d34db33f/84'/0'/0'])"));
        assert_eq!(r[1].spendable, None);
    }

    #[test]
    fn bad_lines_are_counted_by_number() {
        let (r, bad) = parse("\u{feff}{\"type\":\"tx\",\"ref\":\"ab\"}\n\nnot json\n{\"ref\":\"x\"}\n");
        assert_eq!(r.len(), 1);
        assert_eq!(bad, vec![3, 4]);
    }

    #[test]
    fn origins_compare_however_they_are_written() {
        assert_eq!(origin_key("wpkh([D34DB33F/84h/0h/0h])"), origin_key("wpkh([d34db33f/84'/0'/0'])"));
        assert_ne!(origin_key("wpkh([d34db33f/84'/0'/1'])"), origin_key("wpkh([d34db33f/84'/0'/0'])"));
        assert_eq!(origin_key("wpkh(xpub)"), None);
    }
}
