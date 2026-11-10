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
    let feather = 1.0 / ui.ctx().pixels_per_point();
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
            p.add(ribbon(pos2(x0, a_top), a_w, pos2(x1, b_top), b_w, fill, feather));
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
/// curve, as a triangle strip.
///
/// egui does not smooth the edges of a mesh, so a plain strip has stair steps along its long
/// edges. Each of those gets a fringe `feather` points tall that fades from the fill to nothing,
/// the same ramp egui puts on its own shapes: four vertices per column, the outer two clear and
/// the inner two solid, straddling the true edge.
fn ribbon(a: Pos2, aw: f32, b: Pos2, bw: f32, fill: Color32, feather: f32) -> Shape {
    const STEPS: usize = 28;
    let mut mesh = Mesh::default();
    for i in 0..=STEPS {
        let t = i as f32 / STEPS as f32;
        // Smoothstep in y gives the same S as a cubic with flat ends.
        let s = t * t * (3.0 - 2.0 * t);
        let x = a.x + (b.x - a.x) * t;
        let top = a.y + (b.y - a.y) * s;
        let bottom = top + aw + (bw - aw) * s;
        let half = feather / 2.0;
        // A band thinner than the fringe keeps a solid middle rather than turning inside out.
        let inset = half.min((bottom - top) / 2.0);
        mesh.colored_vertex(pos2(x, top - half), Color32::TRANSPARENT);
        mesh.colored_vertex(pos2(x, top + inset), fill);
        mesh.colored_vertex(pos2(x, bottom - inset), fill);
        mesh.colored_vertex(pos2(x, bottom + half), Color32::TRANSPARENT);
        if i > 0 {
            let k = (i as u32) * 4;
            for j in 0..3 {
                let (p, q) = (k - 4 + j, k + j);
                mesh.add_triangle(p, p + 1, q);
                mesh.add_triangle(p + 1, q + 1, q);
            }
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
        let Shape::Mesh(m) = ribbon(pos2(0.0, 0.0), 10.0, pos2(100.0, 50.0), 4.0, Color32::WHITE, 1.0) else { panic!() };
        assert_eq!(m.vertices.len(), 4 * 29);
        assert_eq!(m.indices.len(), 18 * 28);
        assert!(m.indices.iter().all(|&i| (i as usize) < m.vertices.len()));
    }

    #[test]
    fn long_labels_are_clipped() {
        assert_eq!(clip("abcdef", 10), "abcdef");
        assert_eq!(clip("abcdefghijkl", 8), "abcde...");
    }
}

pub fn history(ui: &mut Ui, g: &crate::engine::graph::History) {
    let columns = g.columns().max(1);
    let mut per_col = vec![0usize; columns];
    let mut row_of = Vec::with_capacity(g.nodes.len());
    for n in &g.nodes {
        row_of.push(per_col[n.depth]);
        per_col[n.depth] += 1;
    }
    let rows = per_col.iter().copied().max().unwrap_or(1);
    let width = ui.available_width();
    let height = (rows as f32 * 26.0 + 24.0).max(120.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, WELL);
    let step = ((width - 80.0) / columns as f32).clamp(60.0, 220.0);
    let at = |i: usize| pos2(rect.left() + 40.0 + g.nodes[i].depth as f32 * step, rect.top() + 18.0 + row_of[i] as f32 * 26.0);
    for e in &g.edges {
        let (a, b) = (at(e.from), at(e.to));
        let colour = if e.change { TRACE } else { LEGEND };
        let mid = (a.x + b.x) / 2.0;
        p.add(Shape::CubicBezier(egui::epaint::CubicBezierShape::from_points_stroke(
            [a, pos2(mid, a.y), pos2(mid, b.y), b],
            false,
            Color32::TRANSPARENT,
            egui::Stroke::new(1.5, colour.gamma_multiply(0.7)),
        )));
    }
    let hover = resp.hover_pos();
    let mut tip = None;
    for (i, n) in g.nodes.iter().enumerate() {
        let c = at(i);
        let colour = if n.kyc {
            FAULT
        } else if n.merges > 1 {
            egui_bench::prelude::WARN
        } else if n.risks > 0 {
            egui_bench::prelude::WARN
        } else {
            OK
        };
        let r = 3.0 + (n.merges.min(4) as f32);
        p.circle_filled(c, r, colour);
        if hover.is_some_and(|h| h.distance(c) < 10.0) {
            tip = Some(i);
        }
        let name = if n.label.is_empty() { format!("{}...", &n.txid.to_string()[..8]) } else { clip(&n.label, 18) };
        let galley = p.layout_no_wrap(name, figure(10.5), LEGEND);
        let at_text = c + vec2(r + 5.0, -galley.size().y / 2.0);
        p.rect_filled(Rect::from_min_size(at_text, galley.size()).expand2(vec2(3.0, 1.0)), 2.0, WELL);
        p.galley(at_text, galley, LEGEND);
    }
    if let Some(i) = tip {
        let n = &g.nodes[i];
        let mut line = format!("{}", &n.txid.to_string()[..10]);
        if n.merges > 1 {
            line.push_str(&format!(", merges {} sources", n.merges));
        }
        if n.kyc {
            line.push_str(", KYC source");
        }
        if n.risks > 0 {
            line.push_str(&format!(", {} warnings", n.risks));
        }
        resp.on_hover_text(line);
    }
}

pub fn history_key(ui: &mut Ui) {
    ui.horizontal(|ui| {
        for (c, t) in [(OK, "one source"), (egui_bench::prelude::WARN, "merges coins or has warnings"), (FAULT, "merges a KYC source"), (TRACE, "line is change")] {
            let (dot, _) = ui.allocate_exact_size(vec2(10.0, 14.0), Sense::hover());
            ui.painter().circle_filled(dot.center(), 3.5, c);
            ui.label(egui::RichText::new(t).size(10.5).color(LEGEND));
            ui.add_space(8.0);
        }
    });
}
