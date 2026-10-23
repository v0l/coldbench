//! The open wallet's pages, laid out like Sparrow's: transactions, send, receive, addresses, UTXOs
//! and settings.

use crate::app::{App, Browse, FeePick, Purpose, Recipient, Review, SendForm, Tab, Views, home};
use crate::engine::{Account, SendPlan, network_name};
use crate::units::{self, Unit};
use crate::{flow, widgets};
use crate::work::Op;
use bdk_wallet::KeychainKind;
use bdk_wallet::bitcoin::Network;
use egui::Ui;
use egui_bench::prelude::*;

const DETAIL_H: f32 = 430.0;

pub fn explorer(network: Network, txid: &str) -> String {
    let base = match network {
        Network::Bitcoin => "https://mempool.space",
        Network::Testnet => "https://mempool.space/testnet",
        Network::Testnet4 => "https://mempool.space/testnet4",
        Network::Signet => "https://mempool.space/signet",
        _ => "https://mempool.space",
    };
    format!("{base}/tx/{txid}")
}

fn signed(unit: Unit, sats: i64) -> String {
    let s = unit.format(sats);
    if sats > 0 { format!("+{s}") } else { s }
}

impl App {
    /// Run `f` with the cached views out of `self`, so tabs can read them and change the app.
    fn with_views(&mut self, ui: &mut Ui, f: impl FnOnce(&mut App, &mut Ui, &Views)) {
        let Some(v) = self.views.take() else { return };
        f(self, ui, &v);
        if self.views.is_none() {
            self.views = Some(v);
        }
    }

    /// A one-line label editor for `key` (a txid, an address or an outpoint).
    fn label_box(&mut self, ui: &mut Ui, key: &str) {
        if self.label_key != key {
            self.label_key = key.to_string();
            self.label_edit = self.account().map(|a| a.label(key).to_string()).unwrap_or_default();
        }
        let mut save = false;
        row(ui, "label", |ui| {
            let r = field_then(ui, &mut self.label_edit, "a note only you see", 70.0, |ui| {
                save = ui.add(Key::new("save").outline(READOUT)).clicked();
            });
            save |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        });
        if save {
            let text = self.label_edit.clone();
            let key = key.to_string();
            if let Some(a) = self.account_mut() {
                a.set_label(&key, &text);
                let r = a.save();
                if let Err(e) = r {
                    self.say(false, e);
                }
            }
        }
    }

    // ---- transactions ----

    pub fn transactions(&mut self, ui: &mut Ui) {
        self.with_views(ui, |app, ui, v| {
            let unit = app.unit;
            ui.horizontal_top(|ui| {
                hero(ui, "balance", &unit.format(v.balance.total() as i64), unit.label(), TRACE);
                ui.add_space(24.0);
                readouts(
                    ui,
                    &[
                        ("confirmed", unit.format(v.balance.confirmed as i64), TRACE),
                        ("pending", unit.format(v.balance.pending as i64), TRACE),
                        ("transactions", v.txs.len().to_string(), VALUE),
                    ],
                );
            });
            if v.txs.is_empty() {
                section(ui, "no transactions", "nothing has been paid to this wallet yet", |ui| {
                    note(ui, "Give out an address from the receive page. Payments show here after a sync.", LEGEND);
                });
                return;
            }
            let has_detail = app.selected_tx.is_some_and(|t| v.txs.iter().any(|r| r.txid == t));
            let room = ui.available_height() - if has_detail { DETAIL_H + 8.0 } else { 0.0 } - ROW_H;
            let cols = [("date", 130.0), ("label", 290.0), ("value", 130.0), ("balance", 130.0), ("status", 120.0)];
            let picked = widgets::rows(ui, "txs", &cols, v.txs.len(), room.max(80.0), &|i| Some(v.txs[i].txid) == app.selected_tx, |i, p, r, at| {
                let t = &v.txs[i];
                let date = t.time.map(units::when).unwrap_or_else(|| "-".into());
                at.put(p, r, 0, &date, VALUE);
                let label = if t.label.is_empty() { units::short(&t.txid.to_string(), 10) } else { t.label.clone() };
                at.put(p, r, 1, &label, if t.label.is_empty() { LEGEND } else { VALUE });
                at.put(p, r, 2, &signed(unit, t.delta), if t.delta < 0 { FAULT } else { OK });
                at.put(p, r, 3, &unit.format(t.balance), VALUE);
                let status = if t.confirmations == 0 { "unconfirmed".to_string() } else if t.confirmations < 6 { format!("{} conf", t.confirmations) } else { "confirmed".to_string() };
                at.put(p, r, 4, &status, if t.confirmations == 0 { WARN } else { LEGEND });
            });
            if let Some(i) = picked {
                app.selected_tx = Some(v.txs[i].txid);
            }
            let Some(t) = app.selected_tx.and_then(|id| v.txs.iter().find(|r| r.txid == id)) else { return };
            let network = app.account().map(|a| a.network()).unwrap_or(Network::Bitcoin);
            let outputs = app.account().map(|a| a.outputs_of(&t.tx)).unwrap_or_default();
            let inputs = app.account().map(|a| a.inputs_of(&t.tx)).unwrap_or_default();
            let txid = t.txid.to_string();
            section(ui, "transaction", &units::short(&txid, 12), |ui| {
                ui.set_min_height(DETAIL_H - 40.0);
                reading(ui, "txid", txid.clone());
                let at = match (t.height, t.time) {
                    (Some(h), Some(tm)) => format!("block {h}, {}, {} confirmations", units::when(tm), t.confirmations),
                    (None, Some(tm)) => format!("in the mempool since {}", units::when(tm)),
                    _ => "unconfirmed".into(),
                };
                reading(ui, "status", at);
                reading(ui, "amount", unit.with_unit(t.delta));
                if let Some(f) = t.fee {
                    let vb = t.tx.vsize() as f64;
                    reading(ui, "fee", format!("{} ({:.1} sat/vB)", unit.with_unit(f as i64), f as f64 / vb.max(1.0)));
                }
                // Every input's value is needed to draw it to scale; a coin of someone else's that
                // the wallet never saw leaves a gap, so then only the list is shown.
                if inputs.iter().all(|(_, v, _)| v.is_some()) {
                    let ins: Vec<(u64, String, bool)> = inputs.iter().map(|(a, v, m)| (v.unwrap_or(0), units::short(a, 8), *m)).collect();
                    let outs: Vec<(u64, String, flow::Kind)> = outputs
                        .iter()
                        .map(|(a, v, m)| {
                            // Ours on a payment out is change; on a payment in it is what we got.
                            let k = match (*m, t.delta < 0) {
                                (true, true) => flow::Kind::Change,
                                (true, false) => flow::Kind::Ours,
                                (false, _) => flow::Kind::Pay,
                            };
                            (*v, units::short(a, 8), k)
                        })
                        .collect();
                    let (i, o) = flow::ends_of(&ins, &outs, t.fee);
                    let h = (i.len().max(o.len()) as f32 * 36.0 + 50.0).clamp(150.0, 260.0);
                    flow::flow(ui, &i, &o, h, &|v| unit.format(v as i64));
                    flow::key(ui, &[&i, &o]);
                } else {
                    for (addr, value, mine) in &outputs {
                        let tag = if *mine { "  ours" } else { "" };
                        reading(ui, "output", format!("{} {}{tag}", unit.with_unit(*value as i64), addr));
                    }
                }
                app.label_box(ui, &txid);
                ui.horizontal(|ui| {
                    if ui.add(Key::new("copy txid").outline(VALUE)).clicked() {
                        ui.ctx().copy_text(txid.clone());
                    }
                    if ui.add(Key::new("view in explorer").outline(VALUE)).clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(explorer(network, &txid)));
                    }
                    if Account::can_bump(t) {
                        ui.add_space(12.0);
                        Line::new().legend("new rate").show(ui);
                        ui.allocate_ui(egui::vec2(70.0, KEY_H), |ui| {
                            field(ui, &mut app.bump_rate, "sat/vB");
                        });
                        if ui.add(Key::new("bump fee").solid(READOUT)).on_hover_text("replace it with one that pays a higher fee").clicked() {
                            app.bump(t.txid);
                        }
                    }
                });
            });
        });
    }

    fn bump(&mut self, txid: bdk_wallet::bitcoin::Txid) {
        let Ok(rate) = self.bump_rate.trim().parse::<f64>() else {
            return self.say(false, "type the new fee rate in sat/vB");
        };
        let Some(a) = self.account_mut() else { return };
        match a.bump(txid, rate) {
            Ok(built) => {
                self.review = Some(Review { psbt: built.psbt.clone(), built, tx: None, error: None, replaces: Some(txid) });
                self.tab = Tab::Send;
            }
            Err(e) => self.say(false, e),
        }
    }

    // ---- send ----

    pub fn fee_rate(&self) -> Option<f64> {
        let f = self.fees.as_ref();
        match self.send.fee {
            FeePick::Fast => f.map(|f| f.fast),
            FeePick::Medium => f.map(|f| f.medium),
            FeePick::Slow => f.map(|f| f.slow),
            FeePick::Custom => self.send.custom.trim().parse::<f64>().ok(),
        }
    }

    fn plan(&self) -> Result<SendPlan, String> {
        let a = self.account().ok_or("no wallet open")?;
        let mut recipients = Vec::new();
        let n = self.send.recipients.len();
        for (i, r) in self.send.recipients.iter().enumerate() {
            let which = if n > 1 { format!("recipient {}: ", i + 1) } else { String::new() };
            if r.address.trim().is_empty() {
                return Err(format!("{which}type or paste an address"));
            }
            let addr = a.parse_address(&r.address).map_err(|e| format!("{which}{e}"))?;
            let max = r.max && i == n - 1;
            let value = if max {
                0
            } else {
                self.unit.parse(&r.amount).ok_or_else(|| format!("{which}type an amount in {}", self.unit.label()))?
            };
            recipients.push((addr, value));
        }
        let rate = self.fee_rate().ok_or("pick a fee rate, the estimates have not arrived yet")?;
        Ok(SendPlan {
            recipients,
            max: self.send.recipients.last().is_some_and(|r| r.max),
            sat_per_vb: rate,
            rbf: self.send.rbf,
            only: self.send.only.clone(),
            label: self.send.label.clone(),
            stonewall: self.send.stonewall,
        })
    }

    pub fn demo_stonewall(&mut self) {
        self.send.recipients = vec![Recipient { address: "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq".into(), amount: "0.004".into(), max: false }];
        self.send.fee = FeePick::Custom;
        self.send.custom = "2".into();
        self.send.stonewall = true;
        self.send.label = "demo".into();
        self.tab = Tab::Send;
        match self.plan().and_then(|p| self.account_mut().ok_or("no wallet".to_string())?.build(&p)) {
            Ok(built) => self.review = Some(Review { psbt: built.psbt.clone(), built, tx: None, error: None, replaces: None }),
            Err(e) => self.say(false, e),
        }
    }

    pub fn send_tab(&mut self, ui: &mut Ui) {
        if self.review.is_some() {
            return self.review_page(ui);
        }
        let unit = self.unit;
        let balance = self.views.as_ref().map(|v| v.balance.confirmed + v.balance.pending).unwrap_or(0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(760.0);
            let n = self.send.recipients.len();
            let mut remove = None;
            for i in 0..n {
                let title = if n > 1 { format!("pay to {}", i + 1) } else { "pay to".to_string() };
                section(ui, &title, "an address and how much it gets", |ui| {
                    let last = i == n - 1;
                    let r = &mut self.send.recipients[i];
                    row(ui, "address", |ui| {
                        field(ui, &mut r.address, "bc1q...");
                    });
                    row(ui, &format!("amount ({})", unit.label()), |ui| {
                        if r.max && last {
                            Line::new().set("everything left after the fee").show(ui);
                        } else {
                            field(ui, &mut r.amount, if unit == Unit::Btc { "0.001" } else { "100000" });
                        }
                    });
                    if last {
                        switch(ui, "send all", &mut r.max, "the rest of the wallet", "Spends every coin that is not frozen, or every chosen coin, leaving no change.");
                    }
                    if n > 1 && ui.add(Key::new("remove").outline(FAULT)).clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                self.send.recipients.remove(i);
            }
            if ui.add(Key::new("add recipient").outline(VALUE)).clicked() {
                if let Some(l) = self.send.recipients.last_mut() {
                    l.max = false;
                }
                self.send.recipients.push(Recipient::default());
            }
            section(ui, "fee", "what the miners get", |ui| {
                let f = self.fees.clone();
                let rate = |v: Option<f64>| v.map(crate::app::rate).unwrap_or_else(|| "-".into());
                let labels = [
                    (FeePick::Fast, format!("next block {}", rate(f.as_ref().map(|f| f.fast)))),
                    (FeePick::Medium, format!("3 blocks {}", rate(f.as_ref().map(|f| f.medium)))),
                    (FeePick::Slow, format!("6 blocks {}", rate(f.as_ref().map(|f| f.slow)))),
                    (FeePick::Custom, "custom".to_string()),
                ];
                let options: Vec<(FeePick, &str, Option<usize>)> = labels.iter().map(|(p, l)| (*p, l.as_str(), None)).collect();
                ui.push_id("fee pick", |ui| segments(ui, &mut self.send.fee, &options));
                ui.add_space(4.0);
                if self.send.fee == FeePick::Custom {
                    row(ui, "rate (sat/vB)", |ui| {
                        field(ui, &mut self.send.custom, "2");
                    });
                } else {
                    Line::new().legend("rate").set(rate(self.fee_rate()) + " sat/vB").show(ui);
                }
                switch(ui, "replaceable", &mut self.send.rbf, "allow a fee bump later", "Marks the payment as replaceable (BIP125) so it can be sped up while unconfirmed.");
            });
            section(ui, "optimise for", "what the transaction gives away", |ui| {
                ui.push_id("optimise", |ui| segments(ui, &mut self.send.stonewall, &[(false, "efficiency", None), (true, "privacy", None)]));
                ui.add_space(4.0);
                if self.send.stonewall {
                    note(
                        ui,
                        "A Stonewall: two groups of your coins each pay the amount, one to the recipient and one \
                         back to you, each with its own change. It reads as two people in a coinjoin, so nobody can \
                         tell which output is the payment or assume every input is yours. About twice the fee.",
                        LEGEND,
                    );
                    hint(ui, "Needs one recipient, a set amount, and coins on separate addresses that cover it twice.");
                } else {
                    note(ui, "The fewest coins and the lowest fee. The inputs and the change are plainly yours.", LEGEND);
                }
            });
            section(ui, "label", "kept with the transaction", |ui| {
                field(ui, &mut self.send.label, "what this payment is for");
            });
            section(ui, "coins", "what it is paid from", |ui| match &self.send.only {
                Some(only) => {
                    let total: u64 = self
                        .views
                        .as_ref()
                        .map(|v| v.utxos.iter().filter(|u| only.contains(&u.outpoint)).map(|u| u.value).sum())
                        .unwrap_or(0);
                    Line::new().legend("chosen").set(format!("{} coins, {}", only.len(), unit.with_unit(total as i64))).show(ui);
                    if ui.add(Key::new("let the wallet choose").outline(VALUE)).clicked() {
                        self.send.only = None;
                    }
                }
                None => {
                    Line::new().legend("available").measured(unit.with_unit(balance as i64)).gap(10.0).note("chosen by the wallet").show(ui);
                    hint(ui, "To pick the coins yourself, select them on the utxos page.");
                }
            });
            let plan = self.plan();
            if let Err(e) = &plan {
                status(ui, false, e);
            }
            ui.horizontal(|ui| {
                if ui.add(Key::new("create transaction").solid(READOUT).min_width(160.0).enabled(plan.is_ok())).clicked()
                    && let Ok(p) = &plan {
                        let r = self.account_mut().map(|a| a.build(p));
                        match r {
                            Some(Ok(built)) => self.review = Some(Review { psbt: built.psbt.clone(), built, tx: None, error: None, replaces: None }),
                            Some(Err(e)) => self.say(false, e),
                            None => {}
                        }
                    }
                if ui.add(Key::new("clear").outline(VALUE)).clicked() {
                    self.send = SendForm::default();
                }
            });
        });
    }

    fn review_page(&mut self, ui: &mut Ui) {
        let unit = self.unit;
        let Some(r) = &self.review else { return };
        let signed = Account::signed_inputs(&r.psbt);
        let inputs = r.psbt.inputs.len();
        let stage = if r.tx.is_some() { 2 } else { 1 };
        let built = r.built.clone();
        let error = r.error.clone();
        let ready = r.tx.is_some();
        let replaces = r.replaces;
        let txid = r.psbt.unsigned_tx.compute_txid().to_string();
        let device = self.account().map(|a| a.meta.kind.label()).unwrap_or("device");
        let mut discard = false;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(860.0);
            stage_rail(ui, &["create", "sign", "broadcast"], Some(stage), READOUT, 22.0, false);
            ui.add_space(4.0);
            if built.stonewall {
                status(ui, true, "Stonewall: one of the two equal outputs comes back to you, nobody else can tell which");
            }
            if let Some(n) = &built.note {
                note(ui, n.clone(), WARN);
            }
            if let Some(old) = replaces {
                note(ui, format!("replaces {} with a higher fee", units::short(&old.to_string(), 10)), WARN);
            }
            section(ui, "pays", "check these on the device before you approve", |ui| {
                let sent: u64 = built.outputs.iter().filter(|o| !o.change && !o.decoy).map(|o| o.value).sum();
                ui.horizontal_top(|ui| {
                    hero(ui, "sending", &unit.format(sent as i64), unit.label(), VALUE);
                    ui.add_space(24.0);
                    readouts(
                        ui,
                        &[
                            ("fee", unit.format(built.fee as i64), VALUE),
                            ("rate", format!("{:.1}", built.sat_per_vb), VALUE),
                            ("signed", format!("{signed} of {inputs}"), if ready { OK } else { VALUE }),
                        ],
                    );
                });
                for o in &built.outputs {
                    let tag = if o.decoy { "decoy" } else if o.change { "change" } else { "to" };
                    Line::new().legend(tag).column(ui, 70.0).value(unit.with_unit(o.value as i64)).column(ui, 220.0).value(o.address.clone()).show(ui);
                }
            });
            section(ui, "flow", "how the coins move, widths to scale", |ui| {
                let ins: Vec<(u64, String, bool)> = built.inputs.iter().map(|i| (i.value, units::short(&i.address, 8), true)).collect();
                let outs: Vec<(u64, String, flow::Kind)> = built
                    .outputs
                    .iter()
                    .map(|o| {
                        let k = if o.decoy { flow::Kind::Decoy } else if o.change { flow::Kind::Change } else { flow::Kind::Pay };
                        (o.value, units::short(&o.address, 8), k)
                    })
                    .collect();
                let (i, o) = flow::ends_of(&ins, &outs, Some(built.fee));
                let rows = i.len().max(o.len()) as f32;
                let h = (rows * 40.0 + 60.0).clamp(180.0, 420.0);
                flow::flow(ui, &i, &o, h, &|v| unit.format(v as i64));
                flow::key(ui, &[&i, &o]);
            });
            section(ui, "spends", &format!("{} coins", built.inputs.len()), |ui| {
                for i in &built.inputs {
                    Line::new().legend("from").column(ui, 70.0).value(unit.with_unit(i.value as i64)).column(ui, 220.0).value(i.address.clone()).show(ui);
                }
                if !built.label.is_empty() {
                    reading(ui, "label", built.label.clone());
                }
                reading(ui, "txid", txid.clone());
            });
            if let Some(e) = &error {
                status(ui, false, e);
            } else if ready {
                status(ui, true, "fully signed, nothing has been sent yet");
            }
            ui.horizontal(|ui| {
                if ready {
                    let busy = self.broadcasting();
                    if ui.add(Key::new(if busy { "broadcasting" } else { "broadcast" }).solid(READOUT).min_width(140.0).enabled(!busy)).clicked() {
                        self.broadcast();
                    }
                } else if ui.add(Key::new(format!("sign on {}", device.to_lowercase())).solid(READOUT).min_width(140.0).enabled(self.device.is_none())).clicked()
                    && let Some(r) = &self.review {
                        let psbt = r.psbt.clone();
                        self.start_device(Purpose::Sign, None, Op::Sign(psbt));
                    }
                if ui.add(Key::new("save psbt").outline(VALUE)).on_hover_text("for a Coldcard SD card or another signer").clicked() {
                    self.browser = Some((egui_bench::browser::Browser::new("save the transaction in", home()).folders(true).suffixes(&[".psbt"]), Browse::SavePsbt));
                }
                if ui.add(Key::new("copy psbt").outline(VALUE)).clicked()
                    && let Some(r) = &self.review {
                        ui.ctx().copy_text(r.psbt.to_string());
                        self.say(true, "the PSBT is on the clipboard as base64");
                    }
                if ready && ui.add(Key::new("copy hex").outline(VALUE)).on_hover_text("the raw signed transaction").clicked()
                    && let Some(tx) = self.review.as_ref().and_then(|r| r.tx.as_ref()) {
                        ui.ctx().copy_text(bdk_wallet::bitcoin::consensus::encode::serialize_hex(tx));
                    }
                ui.add_space(12.0);
                if ui.add(Key::new("discard").outline(FAULT)).clicked() {
                    discard = true;
                }
            });
            hint(ui, "Signed PSBTs can also be dropped on the window or pasted with ctrl+V to add their signatures.");
        });
        if discard {
            self.review = None;
        }
    }

    // ---- receive ----

    pub fn receive_tab(&mut self, ui: &mut Ui) {
        let Some(i) = self.current else { return };
        let still = self.shown_receive.as_ref().is_some_and(|r| {
            self.views.as_ref().is_some_and(|v| v.receive.iter().any(|x| x.address == r.address && x.received == 0))
        });
        if !still {
            self.shown_receive = Some(self.accounts[i].receive());
        }
        let Some(addr) = self.shown_receive.clone() else { return };
        let a = &self.accounts[i];
        let network = a.network();
        let script = a.meta.script;
        let path: bdk_wallet::bitcoin::bip32::DerivationPath = a.full_path(KeychainKind::External, addr.index);
        ui.horizontal_top(|ui| {
            egui::Frame::NONE.show(ui, |ui| widgets::qr(ui, &widgets::uri(&addr.address), 260.0));
            ui.add_space(16.0);
            ui.vertical(|ui| {
                ui.set_max_width(560.0);
                section(ui, "address", &format!("{} {}", network_name(network), script.short()), |ui| {
                    Line::new().value(addr.address.clone()).size(15.0).wrapped(ui);
                    reading(ui, "path", format!("m/{}", addr.path));
                    reading(ui, "index", addr.index.to_string());
                    status(ui, true, "never paid, safe to give out");
                });
                section(ui, "label", "who you gave it to", |ui| {
                    let key = addr.address.clone();
                    self.label_box(ui, &key);
                });
                ui.horizontal(|ui| {
                    if ui.add(Key::new("copy").outline(VALUE)).clicked() {
                        ui.ctx().copy_text(addr.address.clone());
                        self.say(true, "address copied");
                    }
                    if ui.add(Key::new("show on device").solid(READOUT).enabled(self.device.is_none())).on_hover_text("check the device shows the same address before giving it out").clicked() {
                        self.start_device(Purpose::Show { address: addr.address.clone() }, None, Op::Show { script, path: path.clone() });
                    }
                    if ui.add(Key::new("next address").outline(VALUE)).on_hover_text("a new one, this one stays watched").clicked() {
                        self.shown_receive = self.account_mut().map(|a| a.another());
                        let _ = self.account_mut().map(|a| a.save());
                    }
                });
                hint(ui, "Use a new address for every payment. The wallet watches 20 unused addresses ahead.");
            });
        });
    }

    // ---- addresses ----

    pub fn addresses_tab(&mut self, ui: &mut Ui) {
        self.with_views(ui, |app, ui, v| {
            let unit = app.unit;
            let mut chain = app.addr_chain;
            segments(
                ui,
                &mut chain,
                &[(KeychainKind::External, "receive", Some(v.receive.len())), (KeychainKind::Internal, "change", Some(v.change.len()))],
            );
            app.addr_chain = chain;
            ui.add_space(4.0);
            let rows = if chain == KeychainKind::External { &v.receive } else { &v.change };
            let selected = app.selected_addr.clone();
            let has_detail = selected.as_ref().is_some_and(|s| rows.iter().any(|r| &r.address == s));
            let room = ui.available_height() - if has_detail { DETAIL_H } else { 0.0 } - ROW_H;
            let cols = [("path", 120.0), ("address", 380.0), ("label", 220.0), ("balance", 130.0), ("used", 60.0)];
            let picked = widgets::rows(ui, "addrs", &cols, rows.len(), room.max(80.0), &|i| selected.as_deref() == Some(&rows[i].address), |i, p, r, at| {
                let a = &rows[i];
                at.put(p, r, 0, &format!("m/{}", a.path), LEGEND);
                at.put(p, r, 1, &a.address, VALUE);
                at.put(p, r, 2, &a.label, VALUE);
                at.put(p, r, 3, &unit.format(a.balance as i64), if a.balance > 0 { TRACE } else { LEGEND });
                at.put(p, r, 4, &a.received.to_string(), if a.received > 0 { VALUE } else { LEGEND });
            });
            if let Some(i) = picked {
                app.selected_addr = Some(rows[i].address.clone());
            }
            let Some(a) = app.selected_addr.as_ref().and_then(|s| rows.iter().find(|r| &r.address == s)).cloned() else { return };
            let Some(acc) = app.account() else { return };
            let script = acc.meta.script;
            let path = acc.full_path(a.keychain, a.index);
            section(ui, "address", &format!("m/{}", a.path), |ui| {
                reading(ui, "address", a.address.clone());
                reading(ui, "balance", unit.with_unit(a.balance as i64));
                reading(ui, "paid", format!("{} times", a.received));
                if a.received > 0 {
                    note(ui, "Already paid. Giving it out again links those payments together.", WARN);
                }
                app.label_box(ui, &a.address);
                ui.horizontal(|ui| {
                    if ui.add(Key::new("copy").outline(VALUE)).clicked() {
                        ui.ctx().copy_text(a.address.clone());
                    }
                    if ui.add(Key::new("show on device").outline(READOUT).enabled(app.device.is_none())).clicked() {
                        app.start_device(Purpose::Show { address: a.address.clone() }, None, Op::Show { script, path: path.clone() });
                    }
                    if ui.add(Key::new("sign a message").outline(VALUE)).clicked() {
                        app.message = Some(crate::app::MessageTool { address: a.address.clone(), ..Default::default() });
                    }
                });
            });
        });
    }

    // ---- utxos ----

    pub fn utxos_tab(&mut self, ui: &mut Ui) {
        self.with_views(ui, |app, ui, v| {
            let unit = app.unit;
            app.selected_utxos.retain(|o| v.utxos.iter().any(|u| u.outpoint == *o));
            let chosen: Vec<&crate::engine::UtxoRow> = v.utxos.iter().filter(|u| app.selected_utxos.contains(&u.outpoint)).collect();
            let total: u64 = chosen.iter().map(|u| u.value).sum();
            ui.horizontal(|ui| {
                Line::new()
                    .legend("coins")
                    .measured(v.utxos.len().to_string())
                    .gap(14.0)
                    .legend("selected")
                    .set(format!("{} ({})", chosen.len(), unit.with_unit(total as i64)))
                    .show(ui);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let any = !chosen.is_empty();
                    if ui.add(Key::new("send selected").solid(READOUT).enabled(any && chosen.iter().all(|u| !u.frozen))).clicked() {
                        app.send.only = Some(chosen.iter().map(|u| u.outpoint).collect());
                        app.tab = Tab::Send;
                    }
                    let all_frozen = any && chosen.iter().all(|u| u.frozen);
                    let label = if all_frozen { "unfreeze" } else { "freeze" };
                    if ui.add(Key::new(label).outline(VALUE).enabled(any)).on_hover_text("frozen coins are never spent").clicked() {
                        let ops: Vec<_> = chosen.iter().map(|u| u.outpoint).collect();
                        if let Some(a) = app.account_mut() {
                            for o in ops {
                                a.lock(o, !all_frozen);
                            }
                            let _ = a.save();
                        }
                    }
                    if ui.add(Key::new("clear").outline(VALUE).enabled(any)).clicked() {
                        app.selected_utxos.clear();
                    }
                });
            });
            if v.utxos.is_empty() {
                section(ui, "no coins", "this wallet holds nothing yet", |ui| {
                    note(ui, "Coins appear here once a payment to one of its addresses is seen. Select them to freeze them or to spend only those.", LEGEND);
                });
                return;
            }
            let single = (chosen.len() == 1).then(|| chosen[0].clone());
            let room = ui.available_height() - if single.is_some() { 150.0 } else { 0.0 } - ROW_H;
            let cols = [("", 26.0), ("output", 200.0), ("address", 330.0), ("label", 200.0), ("value", 130.0), ("conf", 60.0)];
            let picked = widgets::rows(ui, "utxos", &cols, v.utxos.len(), room.max(80.0), &|i| app.selected_utxos.contains(&v.utxos[i].outpoint), |i, p, r, at| {
                let u = &v.utxos[i];
                let on = app.selected_utxos.contains(&u.outpoint);
                let mark = if u.frozen { "f" } else if on { "x" } else { "" };
                at.put(p, r, 0, mark, if u.frozen { WARN } else { READOUT });
                at.put(p, r, 1, &units::short(&u.outpoint.to_string(), 10), LEGEND);
                at.put(p, r, 2, &u.address, if u.change { LEGEND } else { VALUE });
                at.put(p, r, 3, &u.label, VALUE);
                at.put(p, r, 4, &unit.format(u.value as i64), if u.frozen { LEGEND } else { TRACE });
                let c = if u.confirmations == 0 { "-".to_string() } else { u.confirmations.to_string() };
                at.put(p, r, 5, &c, if u.confirmations == 0 { WARN } else { LEGEND });
            });
            if let Some(i) = picked {
                let o = v.utxos[i].outpoint;
                if !app.selected_utxos.remove(&o) {
                    app.selected_utxos.insert(o);
                }
            }
            if let Some(u) = single {
                section(ui, "coin", &u.outpoint.to_string(), |ui| {
                    reading(ui, "state", if u.frozen { "frozen, never spent" } else if u.change { "change" } else { "received" });
                    app.label_box(ui, &u.outpoint.to_string());
                });
            }
            hint(ui, "Click rows to select coins. f marks a frozen coin.");
        });
    }

    // ---- settings ----

    pub fn settings_tab(&mut self, ui: &mut Ui) {
        let Some(i) = self.current else { return };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(820.0);
            let a = &self.accounts[i];
            let meta = a.meta.clone();
            let external = a.descriptor(KeychainKind::External);
            let internal = a.descriptor(KeychainKind::Internal);
            let file = a.file().display().to_string();
            let mut rename = false;
            section(ui, "wallet", "a watch-only copy of the device's keys", |ui| {
                row(ui, "name", |ui| {
                    let r = field_then(ui, &mut self.name_edit, "a name", 70.0, |ui| {
                        rename = ui.add(Key::new("rename").outline(READOUT)).clicked();
                    });
                    rename |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                });
                reading(ui, "device", meta.kind.label());
                reading(ui, "fingerprint", meta.fingerprint.clone());
                reading(ui, "script", meta.script.label());
                reading(ui, "path", format!("m/{}", meta.account_path()));
                reading(ui, "network", network_name(meta.network));
                reading(ui, "file", file);
            });
            if rename && !self.name_edit.trim().is_empty() {
                let name = self.name_edit.trim().to_string();
                let a = &mut self.accounts[i];
                a.meta.name = name;
                let _ = a.save();
            }
            section(ui, "keys", "safe to share with a watch-only wallet, not with strangers", |ui| {
                reading(ui, "xpub", meta.xpub.clone());
                reading(ui, "receive", external.clone());
                reading(ui, "change", internal.clone());
                ui.horizontal(|ui| {
                    if ui.add(Key::new("copy xpub").outline(VALUE)).clicked() {
                        ui.ctx().copy_text(meta.xpub.clone());
                    }
                    if ui.add(Key::new("copy descriptors").outline(VALUE)).on_hover_text("for Sparrow, Bitcoin Core or another wallet").clicked() {
                        ui.ctx().copy_text(format!("{external}\n{internal}\n"));
                    }
                });
            });
            let mut save_server = false;
            let report = self.reports.get(self.accounts[i].file()).cloned();
            let spread = self.server_edit.lines().all(|l| l.trim().is_empty());
            let typed: Vec<_> = self.server_edit.lines().filter(|l| !l.trim().is_empty()).map(|l| (l.trim().to_string(), crate::net::servers::Server::parse(l))).collect();
            section(ui, "servers", "where the history comes from", |ui| {
                row_help(
                    ui,
                    "your servers",
                    "One per line, https://host/api for Esplora or ssl://host:port for Electrum. Leave it empty to split the addresses over every public server.",
                    |ui| {
                        prose(ui, &mut self.server_edit, "empty: the public servers", 3);
                    },
                );
                for (line, parsed) in &typed {
                    if parsed.is_none() {
                        status(ui, false, &format!("not a server: {line}"));
                    }
                }
                row_help(ui, "parallel", "Requests in flight at once across all servers. Each server gets at most 4.", |ui| {
                    ui.add(egui::Slider::new(&mut self.parallel_edit, 1..=32));
                });
                if spread {
                    note(
                        ui,
                        "Each address is asked of one operator only, picked by a secret only this wallet knows, \
                         so no single server sees the whole wallet. Every server still sees your IP address, \
                         and spending links its inputs on chain for everyone.",
                        LEGEND,
                    );
                } else {
                    note(ui, "Your servers see every address of this wallet unless you list several run by different people.", WARN);
                }
                ui.horizontal(|ui| {
                    let ok = typed.iter().all(|(_, p)| p.is_some());
                    save_server = ui.add(Key::new("save and sync").solid(READOUT).enabled(ok)).clicked();
                });
            });
            if let Some(r) = report {
                section(ui, "last sync", &format!("{} addresses over {} servers", r.addresses, r.servers), |ui| {
                    let most = r.spread.values().copied().max().unwrap_or(0);
                    for (op, n) in &r.spread {
                        let share = if r.addresses == 0 { 0.0 } else { *n as f32 / r.addresses as f32 };
                        Line::new().legend(op).column(ui, 150.0).measured(format!("{n} addresses")).column(ui, 300.0).note(format!("{:.0}%", share * 100.0)).show(ui);
                        let _ = most;
                    }
                    reading(ui, "proofs checked", format!("{} confirmations, each against a header with its work", r.verified));
                    for s in &r.skipped {
                        note(ui, format!("skipped {s}"), WARN);
                    }
                });
            }
            let parallel = self.parallel_edit as usize;
            if save_server {
                let a = &mut self.accounts[i];
                let text: String = typed.iter().map(|(l, _)| format!("{l}\n")).collect();
                a.meta.server = text.trim_end().to_string();
                a.meta.parallel = parallel;
                let _ = a.save();
                self.server_edit = self.accounts[i].meta.server.clone();
                self.fees = None;
                self.fees_reset();
                self.sync(i);
            }
            section(ui, "remove", "forget this wallet on this computer", |ui| {
                note(ui, "Only the watch-only copy and its labels are deleted. The coins stay on the device's keys.", LEGEND);
                if ui.add(Key::new("remove wallet").outline(FAULT)).clicked() {
                    self.removing = Some(i);
                }
            });
        });
    }
}
