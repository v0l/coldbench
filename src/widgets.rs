//! Painted pieces the crate does not have: a table whose rows can be clicked, and a QR code.

use egui::{Rect, Sense, Ui, Vec2};
use egui_bench::prelude::{BAND, READOUT, ROW_H, WELL};
use egui_bench::table;

/// A table with a header and rows that answer clicks. `paint(i, painter, rect, x_of_column)`
/// draws row `i`. Returns the row clicked, if any. Scrolls by itself, showing only visible rows.
pub fn rows(
    ui: &mut Ui,
    id: &str,
    columns: &[(&str, f32)],
    n: usize,
    height: f32,
    selected: &dyn Fn(usize) -> bool,
    mut paint: impl FnMut(usize, &egui::Painter, Rect, &Cols),
) -> Option<usize> {
    let width = ui.available_width();
    // Columns shrink together when the window is narrower than they want.
    let want: f32 = columns.iter().map(|(_, w)| w).sum::<f32>() + 12.0;
    let k = (width / want).min(1.0);
    let scaled: Vec<(&str, f32)> = columns.iter().map(|(n, w)| (*n, w * k)).collect();
    let (head, _) = ui.allocate_exact_size(Vec2::new(width, ROW_H), Sense::hover());
    table::header(&ui.painter_at(head), head, &scaled);
    let starts: Vec<(f32, f32)> = scaled
        .iter()
        .scan(6.0, |x, (_, w)| {
            let at = *x;
            *x += w;
            Some((at, *w))
        })
        .collect();
    let mut clicked = None;
    egui::ScrollArea::vertical().id_salt(id).max_height(height).auto_shrink([false, true]).show_rows(
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

/// Where each column of a row sits, after shrinking to fit.
pub struct Cols<'a> {
    starts: &'a [(f32, f32)],
}

impl Cols<'_> {
    /// Put `text` in column `c` of row `r`.
    pub fn put(&self, p: &egui::Painter, r: Rect, c: usize, text: &str, colour: egui::Color32) {
        if let Some((x, w)) = self.starts.get(c) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bech32_is_upper_cased_in_the_uri() {
        assert_eq!(uri("bc1qabc"), "bitcoin:BC1QABC");
        assert_eq!(uri("3Abc"), "bitcoin:3Abc");
    }
}
