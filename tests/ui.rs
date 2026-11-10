use coldbench::app::{App, Tab};
use coldbench::demo;
use egui::epaint::Shape;
use egui::{Vec2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use std::path::PathBuf;

const SIZES: [Vec2; 3] = [vec2(800.0, 600.0), vec2(1024.0, 640.0), vec2(1280.0, 800.0)];

fn seeded(name: &str) -> (PathBuf, bdk_wallet::bitcoin::Txid) {
    let dir = std::env::temp_dir().join(format!("coldbench-ui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (_, txid) = demo::busy(&dir);
    (dir, txid)
}

fn harness(dir: PathBuf, size: Vec2, setup: impl FnOnce(&mut App)) -> Harness<'static, App> {
    let mut h = Harness::builder().with_size(size).with_max_steps(16).build_eframe(move |cc| {
        egui_bench::install(&cc.egui_ctx);
        let mut app = App::new(&cc.egui_ctx, dir);
        setup(&mut app);
        app
    });
    h.run_steps(4);
    h
}

fn galleys_with(h: &Harness<'_, App>, want: impl Fn(&str) -> bool) -> Vec<usize> {
    let mut found = Vec::new();
    for c in &h.output().shapes {
        let mut stack = vec![&c.shape];
        while let Some(s) = stack.pop() {
            match s {
                Shape::Text(t) if want(t.galley.text()) => found.push(t.galley.rows.len()),
                Shape::Vec(v) => stack.extend(v.iter()),
                _ => {}
            }
        }
    }
    found
}

#[test]
fn unconfirmed_fiat_amount_sits_on_one_line() {
    for code in ["EUR", "USD"] {
        let (dir, _) = seeded(&format!("fiat-{code}"));
        for size in SIZES {
            let code = code.to_string();
            let h = harness(dir.clone(), size, move |a| a.fiat = Some(code));
            let want = coldbench::units::fiat(demo::BUSY_INCOMING as i64, demo::BUSY_PRICE_EUR, h.state().fiat.as_deref().unwrap());
            assert!(want.contains("12,345,678.90"), "{want}");
            let rows = galleys_with(&h, |t| t.contains(&want));
            assert!(
                rows.len() >= 2,
                "{size:?}: {want} is not drawn in both the unconfirmed readout and the table"
            );
            assert!(
                rows.iter().all(|r| *r == 1),
                "{size:?} {}: wrapped onto {rows:?} rows",
                h.state().fiat.as_deref().unwrap()
            );
        }
    }
}

#[test]
fn transaction_detail_keys_are_reachable_in_a_small_window() {
    let (dir, txid) = seeded("detail");
    for size in [vec2(800.0, 600.0), vec2(1024.0, 640.0)] {
        let mut h = harness(dir.clone(), size, move |a| {
            a.tab = Tab::Transactions;
            a.selected_tx = Some(txid);
            a.bump_rate = "20".into();
        });
        h.get_by_label("bump fee").scroll_to_me();
        h.run_steps(4);
        let key = h.get_by_label("bump fee").rect();
        assert!(
            key.bottom() <= size.y,
            "{size:?}: the bump fee key is below the window at {key:?}"
        );
        assert!(
            key.height() <= egui_bench::prelude::KEY_H + 0.5,
            "{size:?}: the bump fee key is squeezed onto two lines, {key:?}"
        );
        h.get_by_label("bump fee").click();
        h.run_steps(4);
        assert_eq!(h.state().tab, Tab::Send, "{size:?}: clicking bump fee did nothing, said {:?}", h.state().notice.as_ref().map(|n| &n.1));
        assert!(h.state().review.as_ref().is_some_and(|r| r.replaces == Some(txid)));
    }
}

fn labelled(h: &Harness<'_, App>, name: &str) -> Vec<(usize, egui::Rect)> {
    let mut found = Vec::new();
    for c in &h.output().shapes {
        let mut stack = vec![&c.shape];
        while let Some(s) = stack.pop() {
            match s {
                Shape::Text(t) if t.galley.text().trim().eq_ignore_ascii_case(name) => {
                    found.push((t.galley.rows.len(), egui::Rect::from_min_size(t.pos, t.galley.size())))
                }
                Shape::Vec(v) => stack.extend(v.iter()),
                _ => {}
            }
        }
    }
    found
}

#[test]
fn header_and_utxo_toolbar_keys_stay_on_one_row_in_a_small_window() {
    let (dir, _) = seeded("toolbar");
    for size in [vec2(800.0, 600.0), vec2(1024.0, 640.0)] {
        let h = harness(dir.clone(), size, |a| {
            a.tab = Tab::Utxos;
            let all: Vec<_> = a.account_mut().expect("a wallet is open").utxos().iter().map(|u| u.outpoint).collect();
            a.selected_utxos.extend(all);
        });
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        let mut keys = Vec::new();
        for name in ["add wallet", "clear", "freeze", "send selected"] {
            let drawn = labelled(&h, name);
            assert!(!drawn.is_empty(), "{size:?}: {name} is not drawn");
            for (rows, rect) in drawn {
                assert_eq!(rows, 1, "{size:?}: {name} wraps onto {rows} rows");
                assert!(screen.contains_rect(rect), "{size:?}: {name} at {rect:?} is off screen");
                keys.push((name, rect));
            }
        }
        let mut readouts = Vec::new();
        for c in &h.output().shapes {
            let mut stack = vec![&c.shape];
            while let Some(s) = stack.pop() {
                match s {
                    Shape::Text(t) if t.galley.text().to_lowercase().contains("selected") && t.galley.text().contains("BTC") => {
                        readouts.push(egui::Rect::from_min_size(t.pos, t.galley.size()))
                    }
                    Shape::Vec(v) => stack.extend(v.iter()),
                    _ => {}
                }
            }
        }
        assert!(!readouts.is_empty(), "{size:?}: the selection readout is not drawn");
        for r in readouts {
            assert!(screen.contains_rect(r), "{size:?}: readout at {r:?} is off screen");
            for (name, k) in &keys {
                assert!(!r.intersects(*k), "{size:?}: readout {r:?} overlaps {name} {k:?}");
            }
        }
    }
}

mod fake_esplora {
    use bdk_wallet::bitcoin::consensus::encode::serialize_hex;
    use bdk_wallet::bitcoin::{OutPoint, Transaction, Txid};
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    pub fn serve(txs: Vec<Transaction>) -> (String, Arc<Mutex<Vec<String>>>) {
        let by_id: HashMap<Txid, Transaction> = txs.into_iter().map(|t| (t.compute_txid(), t)).collect();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://127.0.0.1:{}/api", listener.local_addr().unwrap().port());
        let asked = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(path.clone());
                let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
                let body = match parts.as_slice() {
                    ["api", "tx", id, "hex"] => by_id.get(&id.parse::<Txid>().unwrap()).map(serialize_hex),
                    ["api", "tx", id, "outspend", vout] => {
                        let op = OutPoint::new(id.parse().unwrap(), vout.parse().unwrap());
                        Some(match by_id.values().find(|t| t.input.iter().any(|i| i.previous_output == op)) {
                            Some(t) => format!("{{\"spent\":true,\"txid\":\"{}\"}}", t.compute_txid()),
                            None => "{\"spent\":false}".to_string(),
                        })
                    }
                    _ => None,
                };
                let reply = match body {
                    Some(b) => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}", b.len()),
                    None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                };
                let _ = s.write_all(reply.as_bytes());
            }
        });
        (url, asked)
    }
}

fn wire(from: Vec<bdk_wallet::bitcoin::OutPoint>, to: Vec<(bdk_wallet::bitcoin::ScriptBuf, u64)>) -> bdk_wallet::bitcoin::Transaction {
    use bdk_wallet::bitcoin::{Amount, Sequence, Transaction, TxIn, TxOut, Witness, absolute, transaction};
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: from
            .into_iter()
            .map(|o| TxIn { previous_output: o, script_sig: Default::default(), sequence: Sequence::ENABLE_RBF_NO_LOCKTIME, witness: Witness::new() })
            .collect(),
        output: to.into_iter().map(|(s, v)| TxOut { value: Amount::from_sat(v), script_pubkey: s }).collect(),
    }
}

#[test]
fn tracing_from_settings_against_a_fake_server_lands_in_the_transactions_view() {
    use bdk_wallet::bitcoin::hashes::Hash;
    use bdk_wallet::bitcoin::{OutPoint, ScriptBuf, WPubkeyHash};

    let stranger = |n: u8| ScriptBuf::new_p2wpkh(&WPubkeyHash::hash(&[n, 0xdd]));
    let dir = std::env::temp_dir().join(format!("coldbench-ui-trace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut meta = demo::meta("tracer", 3, 5, "http://127.0.0.1:9");
    let mut probe = coldbench::engine::Account::create(meta.clone(), &dir.join("probe")).unwrap();
    let first = probe.receive().address;
    let mine = probe.parse_address(&first).unwrap().script_pubkey();

    let origin = wire(vec![OutPoint::null()], vec![(stranger(99), 10_100_000)]);
    let funding = wire(vec![OutPoint::new(origin.compute_txid(), 0)], vec![(mine, 10_000_000)]);
    let mut value = 10_000_000u64;
    let spend = wire(vec![OutPoint::new(funding.compute_txid(), 0)], vec![(stranger(1), 100_000), (stranger(2), value - 100_500)]);
    value -= 100_500;
    let mut chain = vec![origin.clone(), funding.clone(), spend.clone()];
    let mut prev = OutPoint::new(spend.compute_txid(), 1);
    for step in 0..4u8 {
        let tx = wire(vec![prev], vec![(stranger(10 + step), 100_000 + step as u64), (stranger(20 + step), value - 100_500)]);
        prev = OutPoint::new(tx.compute_txid(), 1);
        value -= 100_500;
        chain.push(tx);
    }
    let (url, asked) = fake_esplora::serve(chain);

    meta.server = url.clone();
    meta.trace_hops = 4;
    let mut account = coldbench::engine::Account::create(meta, &dir.join("wallets")).unwrap();
    account.add_sent(&funding, "salary").unwrap();
    account.add_sent(&spend, "rent").unwrap();
    std::fs::write(dir.join("config.json"), serde_json::json!({ "last": account.file() }).to_string()).unwrap();

    let mut h = Harness::builder().with_size(vec2(1280.0, 900.0)).with_max_steps(16).with_accessibility_check(false).build_eframe(move |cc| {
        egui_bench::install(&cc.egui_ctx);
        let mut app = App::new(&cc.egui_ctx, dir);
        app.tab = Tab::Settings;
        app
    });
    h.run_steps(4);
    h.get_by_label("trace now").click();
    for _ in 0..200 {
        h.run_steps(2);
        if h.state().traced.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let found = h.state().traced.clone().expect("the trace finished");
    assert!(found.iter().any(|f| f.text.contains("peel chain")), "{found:?}");

    h.state_mut().tab = Tab::Transactions;
    h.run_steps(4);
    assert!(!galleys_with(&h, |t| t.contains("peel chain")).is_empty(), "the finding is not drawn in the transactions view");
    let asked = asked.lock().unwrap();
    assert!(asked.iter().any(|p| p.starts_with("/api/tx/")));
    assert!(asked.iter().all(|p| p.starts_with("/api/tx/") || p.starts_with("/api/block-height/")), "{asked:?}");
}
