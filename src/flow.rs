//! The flow of coins through a transaction, drawn the way mempool.space does it: inputs on the
//! left, outputs on the right, every coin a band whose width is its share of the value, flowing
//! into one trunk in the middle and out again. The fee leaves as a band of its own.

use egui::{Color32, Mesh, Pos2, Rect, Sense, Shape, Ui, Vec2, pos2, vec2};
use egui_bench::prelude::{FAULT, LEGEND, OK, READOUT, TRACE, VALUE, WELL, figure};

/// What a side of the diagram shows.
#[derive(Clone, Debug)]
pub struct End {
    pub value: u64,
    pub label: String,
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// One of our coins being spent.
    Ours,
    /// A coin of someone else's.
    Theirs,
    /// Paid to someone.
    Pay,
    /// Back to us as change.
    Change,
    /// Back to us at the same amount as the payment (a Stonewall's other half).
    Decoy,
    Fee,
}

impl Kind {
    fn colour(self) -> Color32 {
        match self {
            Kind::Ours => TRACE,
            Kind::Theirs => LEGEND,
            Kind::Pay => READOUT,
            Kind::Change => TRACE,
            Kind::Decoy => OK,
            Kind::Fee => FAULT,
        }
    }
}

/// The smallest band, in points, so a coin of a few sats still shows.
const MIN_BAND: f32 = 3.0;
/// Space between bands where they meet their end.
const GAP: f32 = 6.0;
const LABEL_W: f32 = 250.0;
/// The least room an end gets, so its label never runs into the next one.
const SLOT: f32 = 16.0;

/// Draw `inputs` flowing to `outputs` (the fee is one of the outputs) in `height` points.
/// `amount` formats a value for the labels. Returns the index of the hovered end, if any:
/// `Ok(i)` for input `i`, `Err(i)` for output `i`.
pub fn flow(ui: &mut Ui, inputs: &[End], outputs: &[End], height: f32, amount: &dyn Fn(u64) -> String) -> Option<Result<usize, usize>> {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, WELL);
    let total: u64 = inputs.iter().map(|e| e.value).sum::<u64>().max(1);
    let inner = rect.shrink2(vec2(LABEL_W + 10.0, 12.0));
    if inner.width() < 80.0 {
        return None;
    }

    // The trunk takes most of the height; each band is its share of it.
    let trunk_h = (inner.height() * 0.72).max(12.0);
    let band = |v: u64| (v as f32 / total as f32 * trunk_h).max(MIN_BAND);
    let mid = inner.center().x;
    // A narrow waist where everything meets, as on mempool.space.
    let trunk = Rect::from_center_size(pos2(mid, inner.center().y), vec2(inner.width() * 0.04, trunk_h));

    let hover = resp.hover_pos();
    let mut hovered = None;

    // Bands stack on their side, centred, and on the trunk in the same order.
    // Each end gets a slot at least SLOT tall; its band sits in the middle of the slot.
    let layout = |ends: &[End]| -> Vec<(f32, f32)> {
        let widths: Vec<f32> = ends.iter().map(|e| band(e.value)).collect();
        let slots: Vec<f32> = widths.iter().map(|w| w.max(SLOT)).collect();
        let span: f32 = slots.iter().sum::<f32>() + GAP * ends.len().saturating_sub(1) as f32;
        // Too many to fit: squeeze everything evenly.
        let squeeze = if span > inner.height() { inner.height() / span } else { 1.0 };
        let mut y = inner.center().y - span * squeeze / 2.0;
        widths
            .iter()
            .zip(&slots)
            .map(|(w, slot)| {
                let top = y + (slot - w) * squeeze / 2.0;
                y += (slot + GAP) * squeeze;
                (top, w * squeeze)
            })
            .collect()
    };
    let at_trunk = |ends: &[End]| -> Vec<(f32, f32)> {
        let widths: Vec<f32> = ends.iter().map(|e| e.value as f32 / total as f32 * trunk_h).collect();
        let mut y = trunk.top();
        widths
            .iter()
            .map(|w| {
                let top = y;
                y += w;
                (top, w.max(0.5))
            })
            .collect()
    };

    let side = |ends: &[End], left: bool, hovered: &mut Option<Result<usize, usize>>| {
        let outer = layout(ends);
        let inner_y = at_trunk(ends);
        let (x0, x1) = if left { (inner.left(), trunk.left()) } else { (trunk.right(), inner.right()) };
        for (i, e) in ends.iter().enumerate() {
            let (oy, ow) = outer[i];
            let (ty, tw) = inner_y[i];
            let (a_top, a_w, b_top, b_w) = if left { (oy, ow, ty, tw) } else { (ty, tw, oy, ow) };
            let lit = hover.is_some_and(|h| {
                let y = (oy, oy + ow);
                let x = if left { h.x < trunk.left() } else { h.x > trunk.right() };
                x && h.y >= y.0 - GAP / 2.0 && h.y <= y.1 + GAP / 2.0
            });
            if lit {
                *hovered = Some(if left { Ok(i) } else { Err(i) });
            }
            let c = e.kind.colour();
            let fill = if lit { c.gamma_multiply(0.85) } else { c.gamma_multiply(0.45) };
            p.add(ribbon(pos2(x0, a_top), a_w, pos2(x1, b_top), b_w, fill));
            // The end cap and its label.
            let cap_x = if left { inner.left() - 3.0 } else { inner.right() };
            p.rect_filled(Rect::from_min_size(pos2(cap_x, oy), vec2(3.0, ow)), 0.0, c);
            let text_c = if lit { VALUE } else { LEGEND };
            let y = oy + ow / 2.0;
            let (align, tx) = if left { (egui::Align2::RIGHT_CENTER, inner.left() - 8.0) } else { (egui::Align2::LEFT_CENTER, inner.right() + 8.0) };
            let line = format!("{}  {}", amount(e.value), e.label);
            p.with_clip_rect(rect).text(pos2(tx, y), align, clip(&line, 34), figure(11.0), text_c);
        }
    };
    side(inputs, true, &mut hovered);
    side(outputs, false, &mut hovered);
    p.rect_filled(trunk, 0.0, VALUE.gamma_multiply(0.18));
    hovered
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 3).collect::<String>() + "..."
    }
}

/// A band from the left edge `a` (top at `a`, `aw` tall) to the right edge `b`, following an S
/// curve, as a triangle strip between the curve of its top and the curve of its bottom.
fn ribbon(a: Pos2, aw: f32, b: Pos2, bw: f32, fill: Color32) -> Shape {
    const STEPS: usize = 28;
    let curve = |y0: f32, y1: f32, t: f32| {
        // Smoothstep in y gives the same S as a cubic with flat ends.
        let s = t * t * (3.0 - 2.0 * t);
        pos2(a.x + (b.x - a.x) * t, y0 + (y1 - y0) * s)
    };
    let mut mesh = Mesh::default();
    for i in 0..=STEPS {
        let t = i as f32 / STEPS as f32;
        mesh.colored_vertex(curve(a.y, b.y, t), fill);
        mesh.colored_vertex(curve(a.y + aw, b.y + bw, t), fill);
        if i > 0 {
            let k = (i as u32) * 2;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k + 1, k);
        }
    }
    Shape::mesh(mesh)
}

/// Lay a description of a transaction out as ends, fee last.
pub fn ends_of(inputs: &[(u64, String, bool)], outputs: &[(u64, String, Kind)], fee: Option<u64>) -> (Vec<End>, Vec<End>) {
    let ins = inputs
        .iter()
        .map(|(v, l, ours)| End { value: *v, label: l.clone(), kind: if *ours { Kind::Ours } else { Kind::Theirs } })
        .collect();
    let mut outs: Vec<End> = outputs.iter().map(|(v, l, k)| End { value: *v, label: l.clone(), kind: *k }).collect();
    if let Some(f) = fee.filter(|f| *f > 0) {
        outs.push(End { value: f, label: "fee".into(), kind: Kind::Fee });
    }
    (ins, outs)
}

/// A row of coloured keys for the kinds that appear in `ends`, in a fixed order.
pub fn key(ui: &mut Ui, ends: &[&[End]]) {
    const ORDER: [Kind; 6] = [Kind::Ours, Kind::Theirs, Kind::Pay, Kind::Change, Kind::Decoy, Kind::Fee];
    let shown: Vec<Kind> = ORDER.into_iter().filter(|k| ends.iter().any(|side| side.iter().any(|e| e.kind == *k))).collect();
    ui.horizontal(|ui| {
        for k in &shown {
            let (r, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), Sense::hover());
            ui.painter().rect_filled(r, 1.0, k.colour());
            let word = match k {
                Kind::Ours => "your coins",
                Kind::Theirs => "others' coins",
                Kind::Pay => "payment",
                Kind::Change => "change",
                Kind::Decoy => "decoy",
                Kind::Fee => "fee",
            };
            ui.label(egui::RichText::new(word).font(figure(11.0)).color(LEGEND));
            ui.add_space(8.0);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fee_goes_last_and_only_when_there_is_one() {
        let (i, o) = ends_of(&[(10, "a".into(), true)], &[(8, "b".into(), Kind::Pay)], Some(2));
        assert_eq!(i.len(), 1);
        assert_eq!(o.last().unwrap().kind, Kind::Fee);
        let (_, o) = ends_of(&[], &[(8, "b".into(), Kind::Pay)], Some(0));
        assert!(o.iter().all(|e| e.kind != Kind::Fee));
    }

    #[test]
    fn a_ribbon_is_a_closed_strip() {
        let Shape::Mesh(m) = ribbon(pos2(0.0, 0.0), 10.0, pos2(100.0, 50.0), 4.0, Color32::WHITE) else { panic!() };
        assert_eq!(m.vertices.len(), 2 * 29);
        assert_eq!(m.indices.len(), 6 * 28);
        assert!(m.indices.iter().all(|&i| (i as usize) < m.vertices.len()));
    }

    #[test]
    fn long_labels_are_clipped() {
        assert_eq!(clip("abcdef", 10), "abcdef");
        assert_eq!(clip("abcdefghijkl", 8), "abcde...");
    }
}
