//! The viewer panel for the selected operator: a Texture operator's picture through shared frames, or a Channel
//! operator's values as a graph.

use rune::{El, LineCap, Shape, SurfaceFit, canvas, col, el, row, text};
use rune_kit::Kit;

use super::{MONO, SIDE_W, SMALL, family_color};
use crate::app::App;
use crate::network;
use crate::viewer::{ChannelView, Viewer};

const VIEWER_H: f32 = 220.0;
const PAD: f32 = 12.0;
const GRAPH_W: f32 = SIDE_W - 2.0 * PAD;
const GRAPH_H: f32 = 130.0;

pub fn viewer(s: &App, k: &Kit) -> El {
    let frame = col().key("viewer").w_full().h(VIEWER_H).padding(PAD).gap(6.0);
    let Some(op) = s.selected.as_deref().and_then(|path| s.op(path)) else {
        return frame.center().child(
            text("Select an operator to view it")
                .font(k.ty.small)
                .color(k.c.text_faint),
        );
    };
    let content = match &s.viewer {
        Some(Viewer::Texture(v)) => {
            let mut picture = el()
                .key("texture")
                .w_full()
                .grow(1.0)
                .radius(k.radius.sm)
                .fill(k.c.bg_base)
                .native_surface(&v.surface, SurfaceFit::Fit);
            if let Some(status) = v.status() {
                picture = picture.center().child(
                    text(status)
                        .font(SMALL)
                        .color(k.c.text_muted)
                        .wrap()
                        .max_w(GRAPH_W - 24.0),
                );
            }
            picture
        }
        Some(Viewer::Channel(v)) => channels(k, &v.view(), family_color(op.family)),
        _ => col().w_full().grow(1.0).center().child(
            text(format!(
                "No viewer for {} operators yet",
                network::family_label(op.family)
            ))
            .font(SMALL)
            .color(k.c.text_faint),
        ),
    };
    let mut frame = frame.child(content);
    if let Some(error) = &op.error {
        frame = frame.child(
            text(error.clone())
                .key("op-error")
                .font(SMALL)
                .color(k.c.danger.fg)
                .wrap(),
        );
    }
    frame
}

fn channels(k: &Kit, view: &ChannelView, color: rune::Color) -> El {
    let mut panel = col().key("channels").w_full().grow(1.0).gap(6.0);
    if view.channels.is_empty() {
        panel = panel.child(text("No samples yet").font(SMALL).color(k.c.text_faint));
    } else {
        let (lo, hi) = graph_range(&view.channels);
        let palette = |i: usize| color.mix(rune::Color::WHITE, (i as f32 * 0.25).min(0.75));
        let shape = view.channels.iter().enumerate().fold(
            Shape::new().line(
                [0.0, GRAPH_H / 2.0],
                [GRAPH_W, GRAPH_H / 2.0],
                1.0,
                LineCap::Butt,
                k.c.border_muted,
            ),
            |shape, (i, (_, values))| {
                let points = graph_points(values, GRAPH_W, GRAPH_H, lo, hi);
                if points.len() < 2 {
                    shape
                } else {
                    shape.stroke(points, 1.5, palette(i))
                }
            },
        );
        panel = panel.child(
            el().key("graph")
                .size(GRAPH_W, GRAPH_H)
                .radius(k.radius.sm)
                .fill(k.c.bg_base)
                .child(canvas(shape).size(GRAPH_W, GRAPH_H))
                .child(
                    text(format!("{hi:.2}"))
                        .font(SMALL)
                        .color(k.c.text_faint)
                        .absolute(4.0, 2.0),
                )
                .child(
                    text(format!("{lo:.2}"))
                        .font(SMALL)
                        .color(k.c.text_faint)
                        .absolute(4.0, GRAPH_H - 16.0),
                ),
        );
        let mut legend = row().gap(10.0).flex_wrap();
        for (i, (name, values)) in view.channels.iter().enumerate() {
            let last = values.last().copied().unwrap_or(0.0);
            legend = legend.child(
                row()
                    .gap(4.0)
                    .center_y()
                    .child(el().size(8.0, 2.0).fill(palette(i)))
                    .child(text(name.clone()).font(SMALL).color(k.c.text_muted))
                    .child(text(format!("{last:.3}")).font(MONO).color(k.c.text_base)),
            );
        }
        panel = panel.child(legend);
    }
    if let Some(error) = &view.error {
        panel = panel.child(text(error.clone()).font(SMALL).color(k.c.danger.fg).wrap());
    }
    panel
}

/// The range a graph shows: symmetric about zero, so zero sits on the middle line, and at least -1..1.
pub fn graph_range(channels: &[(String, Vec<f32>)]) -> (f32, f32) {
    let values = channels
        .iter()
        .flat_map(|(_, v)| v.iter().copied())
        .filter(|v| v.is_finite());
    let (lo, hi) = values.fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| (lo.min(v), hi.max(v)));
    if lo > hi {
        return (-1.0, 1.0);
    }
    (lo.min(-1.0).min(-hi.abs()), hi.max(1.0).max(lo.abs()))
}

/// Values as points across a `w`×`h` graph, oldest at the left, `hi` at the top.
pub fn graph_points(values: &[f32], w: f32, h: f32, lo: f32, hi: f32) -> Vec<[f32; 2]> {
    let n = values.len();
    let span = (hi - lo).max(f32::EPSILON);
    values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = if n > 1 { w * i as f32 / (n - 1) as f32 } else { 0.0 };
            let t = ((v.clamp(lo, hi) - lo) / span).clamp(0.0, 1.0);
            [x, h * (1.0 - t)]
        })
        .collect()
}
