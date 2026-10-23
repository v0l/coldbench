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
    mut paint: impl FnMut(usize, &egui::Painter, Rect, &dyn Fn(usize) -> f32),
) -> Option<usize> {
    let width = ui.available_width();
    let (head, _) = ui.allocate_exact_size(Vec2::new(width, ROW_H), Sense::hover());
    table::header(&ui.painter_at(head), head, columns);
    let starts: Vec<f32> = columns
        .iter()
        .scan(head.left() + 6.0, |x, (_, w)| {
            let at = *x;
            *x += w;
            Some(at)
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
                let left = r.left();
                let at = |c: usize| starts.get(c).copied().map(|x| x - head.left() + left).unwrap_or(left);
                paint(i, &p, r, &at);
                if resp.clicked() {
                    clicked = Some(i);
                }
            }
        },
    );
    clicked
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bech32_is_upper_cased_in_the_uri() {
        assert_eq!(uri("bc1qabc"), "bitcoin:BC1QABC");
        assert_eq!(uri("3Abc"), "bitcoin:3Abc");
    }
}
