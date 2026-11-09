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
        let h = harness(dir.clone(), size, |a| a.tab = Tab::Utxos);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        for name in ["add wallet", "clear", "freeze", "send selected"] {
            let drawn = labelled(&h, name);
            assert!(!drawn.is_empty(), "{size:?}: {name} is not drawn");
            for (rows, rect) in drawn {
                assert_eq!(rows, 1, "{size:?}: {name} wraps onto {rows} rows");
                assert!(screen.contains_rect(rect), "{size:?}: {name} at {rect:?} is off screen");
            }
        }
    }
}
