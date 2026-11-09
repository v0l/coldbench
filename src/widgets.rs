//! Painted pieces the crate does not have: a table whose rows can be clicked, and a QR code.

use egui::{Rect, Sense, Ui, Vec2};
use egui_bench::prelude::{BAND, READOUT, ROW_H, WELL};
use egui_bench::table;

pub fn fixed(ui: &mut Ui, key: egui_bench::prelude::Key) -> egui::Response {
    let size = Vec2::new(key.width(ui), egui_bench::prelude::KEY_H);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    key.paint(ui, rect)
}

/// A table with a header and rows that answer clicks. `paint(i, painter, rect, x_of_column)`
/// draws row `i`. Returns the row clicked, if any. Scrolls by itself, showing only visible rows.
pub struct Give<'a> {
    pub flex: &'a [usize],
    pub spare: &'a [usize],
    pub height: f32,
}

pub fn rows(
    ui: &mut Ui,
    id: &str,
    columns: &[(&str, f32)],
    give: Give,
    n: usize,
    selected: &dyn Fn(usize) -> bool,
    mut paint: impl FnMut(usize, &egui::Painter, Rect, &Cols),
) -> Option<usize> {
    let width = ui.available_width();
    let scaled: Vec<(&str, f32)> = columns.iter().map(|(n, _)| *n).zip(fit(columns, give.flex, give.spare, width)).collect();
    let (head, _) = ui.allocate_exact_size(Vec2::new(width, ROW_H), Sense::hover());
    let shown: Vec<(&str, f32)> = scaled.iter().copied().filter(|(_, w)| *w > 0.0).collect();
    table::header(&ui.painter_at(head), head, &shown);
    let starts: Vec<(f32, f32)> = scaled
        .iter()
        .scan(6.0, |x, (_, w)| {
            let at = *x;
            *x += w;
            Some((at, *w))
        })
        .collect();
    let mut clicked = None;
    egui::ScrollArea::vertical().id_salt(id).max_height(give.height).auto_shrink([false, true]).show_rows(
        ui,
        ROW_H,
        n,
        |ui, range| {
            for i in range {
                let (r, resp) = ui.allocate_exact_size(Vec2::new(width, ROW_H), Sense::click());
                let p = ui.painter_at(r);
                if selected(i) {
                    p.rect_filled(r, 0.0, READOUT.gamma_multiply(0.16));
                } else if resp.hovered() {
                    p.rect_filled(r, 0.0, BAND);
                } else if i % 2 == 1 {
                    p.rect_filled(r, 0.0, WELL.gamma_multiply(0.6));
                }
                paint(i, &p, r, &Cols { starts: &starts });
                if resp.clicked() {
                    clicked = Some(i);
                }
            }
        },
    );
    clicked
}

pub fn column_for<'a>(ui: &Ui, texts: impl Iterator<Item = &'a str>, least: f32) -> f32 {
    let font = egui_bench::theme::figure(11.0);
    texts.fold(least, |w, t| w.max(ui.painter().layout_no_wrap(t.to_owned(), font.clone(), egui::Color32::WHITE).size().x + 10.0))
}

pub fn fit(columns: &[(&str, f32)], flex: &[usize], spare: &[usize], width: f32) -> Vec<f32> {
    const PAD: f32 = 12.0;
    const LEAST: f32 = 70.0;
    let mut w: Vec<f32> = columns.iter().map(|(_, w)| *w).collect();
    let over = |w: &[f32]| w.iter().sum::<f32>() + PAD - width;
    let give: f32 = flex.iter().map(|&i| (w[i] - LEAST).max(0.0)).sum();
    if over(&w) > 0.0 && give > 0.0 {
        let k = (over(&w) / give).min(1.0);
        for &i in flex {
            w[i] -= (w[i] - LEAST).max(0.0) * k;
        }
    }
    for &i in spare {
        if over(&w) > 0.0 {
            w[i] = 0.0;
        }
    }
    let k = ((width - PAD) / w.iter().sum::<f32>()).min(1.0);
    w.iter().map(|x| x * k).collect()
}

/// Where each column of a row sits, after shrinking to fit.
pub struct Cols<'a> {
    starts: &'a [(f32, f32)],
}

impl Cols<'_> {
    /// Put `text` in column `c` of row `r`.
    pub fn put(&self, p: &egui::Painter, r: Rect, c: usize, text: &str, colour: egui::Color32) {
        if let Some((x, w)) = self.starts.get(c).filter(|(_, w)| *w > 0.0) {
            table::cell(p, r, r.left() + x, *w, text, colour);
        }
    }
}

/// A QR code of `text`, `side` points square, drawn by `egui-qr` on its white ground.
pub fn qr(ui: &mut Ui, text: &str, side: f32) {
    use egui::Widget;
    if let Ok(code) = egui_qr::QrCodeWidget::from_data(text.as_bytes()) {
        code.size(side).quiet_zone(3.0).ui(ui);
    }
}

/// What goes in a QR so phones open it in a wallet: `bitcoin:` and the address, bech32 upper
/// cased because that makes the code smaller.
pub fn uri(address: &str) -> String {
    let a = if address.to_lowercase().starts_with("bc1")
        || address.to_lowercase().starts_with("tb1")
        || address.to_lowercase().starts_with("bcrt1")
    {
        address.to_uppercase()
    } else {
        address.to_string()
    };
    format!("bitcoin:{a}")
}

/// The library's `hero`, with what the figure is worth in the chosen currency, small and gray at
/// the right edge under the unit, on the same line as the label so the label keeps its place.
pub fn hero_sub(ui: &mut Ui, label: &str, v: &str, unit: &str, sub: Option<&str>, tint: egui::Color32) {
    use egui_bench::prelude::LEGEND;
    use egui_bench::theme::{self, LEGEND_SIZE};
    const SIZE: f32 = 42.0;
    let figure_g = ui.painter().layout_no_wrap(v.to_owned(), theme::figure(SIZE), tint);
    let unit_g = ui.painter().layout_no_wrap(unit.to_uppercase(), theme::legend_font(14.0), tint.gamma_multiply(0.75));
    let sub_g = sub.map(|t| ui.painter().layout_no_wrap(t.to_owned(), theme::figure(11.5), LEGEND));
    let label_g = ui.painter().layout_no_wrap(label.to_uppercase(), theme::legend_font(LEGEND_SIZE), LEGEND);
    let top_w = figure_g.size().x + 5.0 + unit_g.size().x;
    let under_w = label_g.size().x + 16.0 + sub_g.as_ref().map_or(0.0, |g| g.size().x);
    let row_h = label_g.size().y.max(sub_g.as_ref().map_or(0.0, |g| g.size().y));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(top_w.max(under_w), figure_g.size().y + 1.0 + row_h), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let base = rect.top() + figure_g.size().y;
    let unit_at = egui::pos2(rect.left() + figure_g.size().x + 5.0, base - unit_g.size().y - figure_g.size().y * 0.14);
    let p = ui.painter();
    p.galley(rect.left_top(), figure_g, tint);
    p.galley(unit_at, unit_g, tint);
    p.galley(egui::pos2(rect.left(), base + 1.0), label_g, LEGEND);
    if let Some(g) = sub_g {
        // Right edge of the figure and its unit, not of the box, which may be wider.
        let right = rect.left() + top_w;
        p.galley(egui::pos2(right - g.size().x, base + 1.0 + (row_h - g.size().y) / 2.0), g, LEGEND);
    }
}

pub fn readout_sub(ui: &mut Ui, label: &str, v: &str, sub: Option<&str>, tint: egui::Color32) {
    use egui_bench::prelude::{LEGEND, READOUT_W};
    use egui_bench::theme::{self, LEGEND_SIZE};
    let label_g = ui.painter().layout_no_wrap(label.to_uppercase(), theme::legend_font(LEGEND_SIZE), LEGEND);
    let value_g = ui.painter().layout_no_wrap(v.to_owned(), theme::figure(17.0), tint);
    let sub_g = sub.map(|t| ui.painter().layout_no_wrap(t.to_owned(), theme::figure(11.5), LEGEND));
    let w = [Some(&label_g), Some(&value_g), sub_g.as_ref()].into_iter().flatten().fold(READOUT_W, |w, g| w.max(g.size().x));
    let h = label_g.size().y + 2.0 + value_g.size().y + sub_g.as_ref().map_or(0.0, |g| g.size().y + 1.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let p = ui.painter();
    let (lh, vh) = (label_g.size().y, value_g.size().y);
    p.galley(rect.left_top(), label_g, LEGEND);
    p.galley(egui::pos2(rect.left(), rect.top() + lh + 2.0), value_g, tint);
    if let Some(g) = sub_g {
        p.galley(egui::pos2(rect.left(), rect.top() + lh + 2.0 + vh + 1.0), g, LEGEND);
    }
}

pub fn wrapping<R>(ui: &mut Ui, gap: f32, body: impl FnOnce(&mut Ui) -> R) -> R {
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true), |ui| {
        ui.spacing_mut().item_spacing = egui::vec2(gap, 8.0);
        body(ui)
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixed_key_keeps_its_measured_width_in_a_narrow_row() {
        use egui_bench::prelude::Key;
        let ctx = egui::Context::default();
        egui_bench::install(&ctx);
        let mut wide = 0.0;
        let mut narrow = 0.0;
        let mut out = ctx.run_ui(Default::default(), |ui| {
            {
                wide = fixed(ui, Key::new("add wallet")).rect.width();
                ui.allocate_ui(Vec2::new(20.0, 40.0), |ui| {
                    narrow = fixed(ui, Key::new("add wallet")).rect.width();
                });
            }
        });
        out.textures_delta.clear();
        assert!(wide > 60.0);
        assert_eq!(wide, narrow);
    }

    #[test]
    fn bech32_is_upper_cased_in_the_uri() {
        assert_eq!(uri("bc1qabc"), "bitcoin:BC1QABC");
        assert_eq!(uri("3Abc"), "bitcoin:3Abc");
    }

    const TXS: [(&str, f32); 6] = [("date", 140.0), ("label", 290.0), ("value", 130.0), ("EUR", 110.0), ("balance", 130.0), ("status", 100.0)];

    #[test]
    fn amount_columns_keep_their_width_while_the_label_gives() {
        let w = fit(&TXS, &[1], &[4], 760.0);
        assert_eq!(&w[2..], &[130.0, 110.0, 130.0, 100.0]);
        assert!(w.iter().sum::<f32>() + 12.0 <= 760.0 + 0.01);
        assert!(w[1] >= 70.0);
    }

    #[test]
    fn a_narrow_table_drops_the_spare_column_before_cutting_amounts() {
        let w = fit(&TXS, &[1], &[4], 600.0);
        assert_eq!(w[4], 0.0);
        assert_eq!(&w[2..4], &[130.0, 110.0]);
        assert!(w.iter().sum::<f32>() + 12.0 <= 600.0 + 0.01);
    }

    #[test]
    fn every_column_shrinks_once_nothing_else_can_give() {
        let cols = [("date", 140.0), ("label", 170.0), ("value", 130.0)];
        let w = fit(&cols, &[1], &[], 300.0);
        assert!((w.iter().sum::<f32>() + 12.0 - 300.0).abs() < 0.01);
        assert!(w[0] < 140.0 && w[2] < 130.0);
    }

    #[test]
    fn a_wide_table_keeps_what_it_asked_for() {
        let cols = [("a", 100.0), ("b", 200.0)];
        assert_eq!(fit(&cols, &[1], &[0], 2000.0), vec![100.0, 200.0]);
    }
}
