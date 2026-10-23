//! Dialogs: adding a wallet, the device prompt, messages, removing a wallet, files.

use crate::app::{App, Browse, Purpose, home};
use crate::engine::{Account, network_name};
use crate::work::{Ask, Op};
use bdk_wallet::bitcoin::Network;
use bdk_wallet::bitcoin::bip32::DerivationPath;
use egui::Color32;
use egui_bench::browser::Picked;
use egui_bench::prelude::*;
use hww::{Kind, Notice, Passphrase, Script};

fn backdrop() -> Color32 {
    Color32::from_black_alpha(150)
}

impl App {
    pub fn new_wallet_modal(&mut self, ctx: &egui::Context) {
        if self.new_wallet.is_none() || self.device.is_some() {
            return;
        }
        let (mut close, mut go) = (false, false);
        let devices = self.devices.clone();
        let r = egui::Modal::new(egui::Id::new("new wallet")).backdrop_color(backdrop()).show(ctx, |ui| {
            ui.set_width(500.0);
            modal_title(ui, "add a wallet from a hardware signer");
            let n = self.new_wallet.as_mut().expect("checked above");
            section(ui, "device", "plugged in and unlocked, or ready to unlock", |ui| {
                if devices.is_empty() {
                    status(ui, false, "looking for a Jade, Coldcard or Trezor");
                    hint(ui, "On Linux the udev rules for each device must be installed (HWI ships them).");
                } else {
                    let mut pick = n.device.unwrap_or(usize::MAX);
                    choice(ui, "device", &mut pick, devices.iter().enumerate().map(|(i, d)| (i, d.name.clone())));
                    if pick != usize::MAX {
                        n.device = Some(pick);
                    }
                }
            });
            ui.add_space(8.0);
            section(ui, "account", "which keys on the device the wallet watches", |ui| {
                row(ui, "name", |ui| {
                    field(ui, &mut n.name, "my jade");
                });
                row(ui, "network", |ui| {
                    choice(
                        ui,
                        "network",
                        &mut n.network,
                        [Network::Bitcoin, Network::Testnet, Network::Testnet4, Network::Signet, Network::Regtest]
                            .map(|x| (x, network_name(x).to_string())),
                    );
                });
                row(ui, "script", |ui| {
                    choice(ui, "script", &mut n.script, Script::ALL.map(|s| (s, s.label().to_string())));
                });
                row_help(ui, "account", "0 unless you keep several accounts on one seed.", |ui| {
                    field(ui, &mut n.account, "0");
                });
                let path = hww::account_path(n.script, n.network, n.account.trim().parse().unwrap_or(0));
                reading(ui, "path", format!("m/{path}"));
            });
            if let Some(e) = &n.error {
                ui.add_space(4.0);
                status(ui, false, e);
            }
            ui.add_space(8.0);
            section(ui, "encryption", "optional: a password that locks the wallet file on this computer", |ui| {
                row(ui, "password", |ui| {
                    secret(ui, &mut n.password);
                });
                row(ui, "again", |ui| {
                    secret(ui, &mut n.confirm);
                });
                if !n.password.is_empty() {
                    note(ui, "Asked for each time the app opens. There is no way to recover a forgotten password; the wallet can be added again from the device.", LEGEND);
                }
            });
            let matches = n.password == n.confirm;
            if !matches {
                status(ui, false, "the two passwords are different");
            }
            let account_ok = n.account.trim().parse::<u32>().is_ok_and(|a| a < 0x8000_0000);
            footer(ui, |ui| {
                let ready = n.device.is_some() && account_ok && matches;
                if ui.add(Key::new("read keys").solid(READOUT).min_width(100.0).enabled(ready)).clicked() {
                    go = true;
                }
                if ui.add(Key::new("cancel").outline(VALUE).min_width(100.0)).clicked() {
                    close = true;
                }
            });
        });
        if r.should_close() {
            close = true;
        }
        if go {
            if let Some(mut n) = self.new_wallet.take() {
                n.error = None;
                let chosen = n.device.and_then(|i| self.devices.get(i)).cloned();
                let op = Op::Identify { script: n.script, account: n.account.trim().parse().unwrap_or(0) };
                self.start_device(Purpose::Import(n), chosen, op);
            }
        } else if close {
            self.new_wallet = None;
        }
    }

    pub fn unlock_modal(&mut self, ctx: &egui::Context) {
        let Some(u) = &mut self.unlock else { return };
        let name = u.file.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let (mut close, mut go) = (false, false);
        let r = egui::Modal::new(egui::Id::new("unlock")).backdrop_color(backdrop()).show(ctx, |ui| {
            ui.set_width(390.0);
            modal_title(ui, &format!("unlock {name}"));
            row(ui, "password", |ui| {
                let resp = secret(ui, &mut u.password);
                if !resp.has_focus() && u.error.is_none() && u.password.is_empty() {
                    resp.request_focus();
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    go = true;
                }
            });
            if let Some(e) = &u.error {
                ui.add_space(4.0);
                status(ui, false, e);
            }
            footer(ui, |ui| {
                if ui.add(Key::new("unlock").solid(READOUT).min_width(100.0).enabled(!u.password.is_empty())).clicked() {
                    go = true;
                }
                if ui.add(Key::new("not now").outline(VALUE).min_width(100.0)).clicked() {
                    close = true;
                }
            });
        });
        if r.should_close() {
            close = true;
        }
        if go {
            let file = u.file.clone();
            match Account::load_with(&file, Some(&u.password)) {
                Ok(a) => {
                    self.unlock = None;
                    self.locked.retain(|p| *p != file);
                    self.accounts.push(a);
                    let i = self.accounts.len() - 1;
                    self.tab = crate::app::Tab::Transactions;
                    self.select(i);
                }
                Err(e) => {
                    u.error = Some(e);
                    u.password.clear();
                }
            }
        } else if close {
            self.unlock = None;
        }
    }

    pub fn device_modal(&mut self, ctx: &egui::Context) {
        let Some(job) = &mut self.device else { return };
        let kind = job.kind;
        let label = kind.map(Kind::label).unwrap_or("device");
        let mut cancel = false;
        let mut answer_pin: Option<Option<String>> = None;
        let mut answer_pass: Option<Option<Passphrase>> = None;
        egui::Modal::new(egui::Id::new("device")).backdrop_color(backdrop()).show(ctx, |ui| {
            ui.set_width(420.0);
            let title = match &job.purpose {
                Purpose::Import(_) => "reading the keys",
                Purpose::Show { .. } => "showing the address",
                Purpose::Sign => "signing",
                Purpose::Message => "signing the message",
            };
            modal_title(ui, &format!("{title} on the {}", label.to_lowercase()));
            match &job.ask {
                Some(Ask::PinMatrix(_)) => {
                    section(ui, "pin", "press the positions shown on the device", |ui| {
                        let grid = [[7, 8, 9], [4, 5, 6], [1, 2, 3]];
                        for row in grid {
                            ui.horizontal(|ui| {
                                for d in row {
                                    if ui.add(Key::new("*").outline(VALUE).min_width(60.0).id_salt(("pin", d))).clicked() {
                                        job.pin.push(char::from(b'0' + d as u8));
                                    }
                                }
                            });
                        }
                        Line::new().legend("entered").set("*".repeat(job.pin.len())).show(ui);
                        ui.horizontal(|ui| {
                            if ui.add(Key::new("enter").solid(READOUT).enabled(!job.pin.is_empty())).clicked() {
                                answer_pin = Some(Some(std::mem::take(&mut job.pin)));
                            }
                            if ui.add(Key::new("clear").outline(VALUE)).clicked() {
                                job.pin.clear();
                            }
                        });
                    });
                }
                Some(Ask::Passphrase(_)) => {
                    section(ui, "passphrase", "the extra word that picks a hidden wallet", |ui| {
                        secret(ui, &mut job.passphrase);
                        hint(ui, "Leave it empty for the standard wallet. A different passphrase opens a different wallet.");
                        ui.horizontal(|ui| {
                            if ui.add(Key::new("use this").solid(READOUT)).clicked() {
                                answer_pass = Some(Some(Passphrase::Host(std::mem::take(&mut job.passphrase))));
                            }
                            if ui.add(Key::new("type it on the device").outline(VALUE)).clicked() {
                                answer_pass = Some(Some(Passphrase::Device));
                            }
                        });
                    });
                }
                None => {
                    let (lit, text) = match &job.notice {
                        Notice::Working => (false, format!("talking to the {label}")),
                        Notice::PinOnDevice => (true, kind.map(Kind::pin_help).unwrap_or("Enter the PIN on the device.").to_string()),
                        Notice::PassphraseOnDevice => (true, format!("Type the passphrase on the {label}.")),
                        Notice::Confirm(t) => (true, format!("{}.", capital(t))),
                    };
                    section(ui, if lit { "on the device" } else { "working" }, "", |ui| {
                        let r = ui.available_rect_before_wrap();
                        let w = r.width();
                        let rect = egui::Rect::from_min_size(r.min, egui::vec2(w, KEY_H));
                        ui.allocate_rect(rect, egui::Sense::hover());
                        let t = job.started.elapsed().as_secs_f32();
                        busy(ui, rect, None, if lit { "waiting for you" } else { "working" }, if lit { READOUT } else { TRACE });
                        ui.add_space(4.0);
                        note(ui, text, if lit { VALUE } else { LEGEND });
                        let _ = t;
                    });
                }
            }
            footer(ui, |ui| {
                if ui.add(Key::new("cancel").outline(FAULT).min_width(100.0)).clicked() {
                    cancel = true;
                }
            });
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        if let Some(a) = answer_pin
            && let Some(Ask::PinMatrix(tx)) = job.ask.take() {
                let _ = tx.send(a);
            }
        if let Some(a) = answer_pass
            && let Some(Ask::Passphrase(tx)) = job.ask.take() {
                let _ = tx.send(a);
            }
        if cancel && job.id == 0 {
            // The preview has no job behind it.
            self.device = None;
            return;
        }
        if cancel {
            job.cancel.cancel();
            // Dropping a question answers it with a cancel.
            job.ask = None;
        }
        // Escape does not close this one: the job decides when it is over.
    }

    pub fn message_modal(&mut self, ctx: &egui::Context) {
        if self.message.is_none() || self.device.is_some() {
            return;
        }
        let mut close = false;
        let mut sign = false;
        let mut verify = false;
        let network = self.account().map(|a| a.network()).unwrap_or(Network::Bitcoin);
        let has_wallet = self.current.is_some();
        let r = egui::Modal::new(egui::Id::new("message")).backdrop_color(backdrop()).show(ctx, |ui| {
            ui.set_width(520.0);
            modal_title(ui, "sign or verify a message");
            let m = self.message.as_mut().expect("checked above");
            section(ui, "message", "BIP137, what Sparrow and Electrum use", |ui| {
                row(ui, "address", |ui| {
                    field(ui, &mut m.address, "an address of this wallet to sign");
                });
                row(ui, "message", |ui| {
                    prose(ui, &mut m.message, "the exact text, every character counts", 4);
                });
                row(ui, "signature", |ui| {
                    prose(ui, &mut m.signature, "base64", 3);
                });
            });
            if let Some((ok, text)) = &m.result {
                status(ui, *ok, text);
            }
            footer(ui, |ui| {
                if ui.add(Key::new("close").outline(VALUE).min_width(90.0)).clicked() {
                    close = true;
                }
                if ui.add(Key::new("verify").outline(READOUT).min_width(90.0).enabled(!m.signature.trim().is_empty())).clicked() {
                    verify = true;
                }
                if ui.add(Key::new("sign on device").solid(READOUT).min_width(120.0).enabled(has_wallet && !m.message.is_empty())).clicked() {
                    sign = true;
                }
            });
        });
        if r.should_close() {
            close = true;
        }
        if verify {
            let m = self.message.as_mut().expect("open");
            m.result = Some(match crate::engine::verify_message(&m.address, &m.message, &m.signature, network) {
                Ok(true) => (true, "the signature is good for this address".into()),
                Ok(false) => (false, "the signature does not match this address and message".into()),
                Err(e) => (false, e),
            });
        }
        if sign {
            let m = self.message.as_ref().expect("open");
            let address = m.address.trim().to_string();
            let text = m.message.clone();
            let Some(a) = self.account() else { return };
            match a.locate(&address) {
                Some((keychain, index)) => {
                    let path: DerivationPath = a.full_path(keychain, index);
                    let script = a.meta.script;
                    self.start_device(Purpose::Message, None, Op::Message { script, path, address, text });
                }
                None => {
                    let m = self.message.as_mut().expect("open");
                    m.result = Some((false, "that address is not one of this wallet's".into()));
                }
            }
        }
        if close {
            self.message = None;
        }
    }

    pub fn remove_modal(&mut self, ctx: &egui::Context) {
        let Some(i) = self.removing else { return };
        let name = self.accounts[i].meta.name.clone();
        let (mut close, mut remove) = (false, false);
        let r = egui::Modal::new(egui::Id::new("remove")).backdrop_color(backdrop()).show(ctx, |ui| {
            ui.set_width(390.0);
            modal_title(ui, &format!("remove {name}"));
            note(ui, "The watch-only file and its labels are deleted from this computer. The device and its coins are not touched, and the wallet can be added again.", LEGEND);
            footer(ui, |ui| {
                if ui.add(Key::new("remove").solid(FAULT).min_width(100.0)).clicked() {
                    remove = true;
                }
                if ui.add(Key::new("keep").outline(VALUE).min_width(100.0)).clicked() {
                    close = true;
                }
            });
        });
        if r.should_close() {
            close = true;
        }
        if remove {
            let a = self.accounts.remove(i);
            let _ = std::fs::remove_file(a.file());
            self.removing = None;
            self.current = None;
            self.views = None;
            if !self.accounts.is_empty() {
                self.select(0);
            } else {
                self.save_config();
            }
            self.say(true, format!("removed {name}"));
        } else if close {
            self.removing = None;
        }
    }

    pub fn browser_modal(&mut self, ctx: &egui::Context) {
        let Some((browser, what)) = &mut self.browser else { return };
        let what = *what;
        let picked = browser.show(ctx);
        match (picked, what) {
            (Picked::Browsing, _) => return,
            (Picked::Cancelled, _) => {}
            (Picked::File(p), Browse::LoadPsbt) => match std::fs::read(&p) {
                Ok(b) => self.load_psbt(&b),
                Err(e) => self.say(false, format!("{}: {e}", p.display())),
            },
            (Picked::File(p), Browse::ImportLabels) => match std::fs::read(&p) {
                Ok(b) => self.import_labels(&b),
                Err(e) => self.say(false, format!("{}: {e}", p.display())),
            },
            (Picked::Folder(dir), Browse::ExportLabels) | (Picked::File(dir), Browse::ExportLabels) => {
                let dir = if dir.is_dir() { dir } else { dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(home) };
                self.export_labels(&dir);
            }
            (Picked::Folder(dir), Browse::SavePsbt) | (Picked::File(dir), Browse::SavePsbt) => {
                let dir = if dir.is_dir() { dir } else { dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(home) };
                if let Some(r) = &self.review {
                    let name = format!("{}.psbt", &r.psbt.unsigned_tx.compute_txid().to_string()[..12]);
                    let path = dir.join(name);
                    match std::fs::write(&path, r.psbt.serialize()) {
                        Ok(()) => self.say(true, format!("saved {}", path.display())),
                        Err(e) => self.say(false, format!("{}: {e}", path.display())),
                    }
                }
            }
            _ => {}
        }
        self.browser = None;
    }
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}
