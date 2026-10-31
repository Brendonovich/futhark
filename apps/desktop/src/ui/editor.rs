//! The network editor: the current component's operators as nodes on a pannable, zoomable canvas, with wires.
//!
//! The world sits under one `live_offset` and `live_scale` (the camera), so panning and zooming move it without
//! re-running the view, except when previews turn on or off.

use futhark_protocol::{Family, Op};
use rune::{
    Color, Drag, El, LineCap, MagnifyEvent, Path, Phase, PointerButton, PointerEvent, Shape, SurfaceFit, Tx,
    WheelEvent, canvas, col, el, row, text,
};
use rune_kit::Kit;

use super::{SMALL, SMALL_MEDIUM, family_color};
use crate::app::App;
use crate::geometry::{self, CONNECTOR, NODE_H, NODE_W, PREVIEW_H, TITLE_H};
use crate::network;
use crate::previews::Preview;

pub fn editor(s: &App, k: &Kit) -> El {
    let nodes = s.nodes();
    let mut world = el()
        .key("world")
        .absolute(0.0, 0.0)
        .size(1.0, 1.0)
        .origin(0.0, 0.0)
        .live_offset(&s.live_pan)
        .live_scale(&s.live_zoom);
    if let Some(wires) = wires(s, &nodes) {
        world = world.child(wires);
    }
    world = world.children(nodes.iter().map(|op| node(s, k, op)));
    let mut canvas = el()
        .key("canvas")
        .grow(1.0)
        .h_full()
        .min_w(0.0)
        .clip_soft(0.0)
        .fill(k.c.bg_base)
        .on_layout(|s: &mut App, r| s.set_viewport([r[2], r[3]]))
        .on_wheel(|s: &mut App, e: WheelEvent, tx: &mut Tx| {
            let camera = if e.mods.cmd || e.mods.ctrl {
                s.camera.zoomed_about([e.x, e.y], (1.0 - e.dy * 0.01).clamp(0.5, 2.0))
            } else {
                s.camera.panned([e.dx, e.dy])
            };
            if !s.set_camera(camera) {
                tx.live_only();
            }
            true
        })
        .on_magnify(|s: &mut App, e: MagnifyEvent, tx: &mut Tx| {
            if !s.set_camera(s.camera.zoomed_about([e.x, e.y], 1.0 + e.delta)) {
                tx.live_only();
            }
            true
        })
        .pointer_buttons([PointerButton::Primary, PointerButton::Middle])
        .on_drag(|s: &mut App, d: &mut Drag, tx: &mut Tx| match d.phase {
            Phase::Down => {
                s.pan_start();
                tx.live_only();
            }
            Phase::Move => {
                if !s.pan_to([d.dx, d.dy]) {
                    tx.live_only();
                }
            }
            Phase::Up => {
                if d.cancelled() {
                    s.pan_to([0.0, 0.0]);
                } else if d.dx.abs() < 3.0 && d.dy.abs() < 3.0 {
                    // A click on the background.
                    s.select(None);
                }
            }
        })
        .on_secondary_press_local(|s: &mut App, e: PointerEvent, _| {
            s.open_add_menu(Some([e.window_x, e.window_y]), Some([e.x, e.y]));
        })
        .child(world);
    if nodes.is_empty() {
        canvas = canvas.child(
            col()
                .key("empty")
                .absolute(0.0, 0.0)
                .w_full()
                .h_full()
                .center()
                .inert()
                .child(
                    text("An empty component. Right-click or press Tab to add an operator.")
                        .font(k.ty.ui)
                        .color(k.c.text_faint),
                ),
        );
    }
    canvas
}

/// Every wire between the nodes, plus one being dragged, in one shape.
fn wires(s: &App, nodes: &[&Op]) -> Option<El> {
    let find = |path: &str| nodes.iter().find(|op| op.path == path).copied();
    let picked = s.wire_drag.as_ref().and_then(|w| w.picked_up.clone());
    let mut curves = Vec::new();
    for op in nodes {
        let slots = s.input_slots(op).len();
        for (i, input) in op.inputs.iter().enumerate() {
            let Some(source) = input.as_ref().and_then(|name| find(&network::join(&s.component, name))) else {
                continue;
            };
            if picked.as_ref().is_some_and(|(to, slot)| *to == op.path && *slot == i) {
                continue;
            }
            let from = geometry::output_point(s.position(source));
            let to = geometry::input_point(s.position(op), i, slots);
            curves.push((geometry::wire(from, to), family_color(source.family)));
        }
    }
    if let Some(w) = &s.wire_drag
        && let Some(source) = find(&w.from)
    {
        let from = geometry::output_point(s.position(source));
        curves.push((geometry::wire(from, w.end), family_color(source.family).alpha(0.7)));
    }
    let controls: Vec<_> = curves.iter().map(|(c, _)| *c).collect();
    let b = geometry::wires_bounds(&controls)?;
    let local = |p: [f32; 2]| [p[0] - b[0], p[1] - b[1]];
    let shape = curves.iter().fold(Shape::new(), |shape, (c, color)| {
        let path = Path::new()
            .move_to(local(c[0]))
            .cubic_to(local(c[1]), local(c[2]), local(c[3]));
        shape.stroke_path(&path, 2.0, LineCap::Round, *color)
    });
    Some(
        canvas(shape)
            .key("wires")
            .absolute(b[0], b[1])
            .size((b[2] - b[0]).max(1.0), (b[3] - b[1]).max(1.0)),
    )
}

fn node(s: &App, k: &Kit, op: &Op) -> El {
    let pos = s.position(op);
    let selected = s.selected.as_deref() == Some(op.path.as_str());
    let color = family_color(op.family);
    let path = op.path.clone();
    let open = op.family == Family::Component;
    let enter = op.path.clone();
    let title = row()
        .key("title")
        .w_full()
        .h(TITLE_H)
        .px(6.0)
        .gap(5.0)
        .center_y()
        .fill(if selected { k.c.layer_04 } else { k.c.layer_03 })
        .child(el().size(6.0, 6.0).radius(3.0).fill(color))
        .child(
            text(network::name(&op.path).to_string())
                .font(SMALL_MEDIUM)
                .color(k.c.text_base)
                .truncate()
                .grow(1.0)
                .min_w(0.0),
        );
    let title = match &op.error {
        Some(_) => title.child(text("!").font(SMALL_MEDIUM).color(k.c.danger.icon)),
        None => title,
    };
    let mut node = col()
        .key(format!("node:{}", op.path))
        .absolute(pos[0], pos[1])
        .size(NODE_W, NODE_H)
        .radius(5.0)
        .clip_rounded(5.0)
        .fill(k.c.layer_02)
        .border(
            if selected { 2.0 } else { 1.0 },
            if selected { k.c.border_focus } else { k.c.border_base },
        )
        .child(preview(s, k, op))
        .child(title)
        .on_drag(move |s: &mut App, d: &mut Drag, _: &mut Tx| match d.phase {
            Phase::Down => s.select(Some(path.clone())),
            Phase::Move => s.drag_node(&path, [d.dx, d.dy]),
            Phase::Up if d.cancelled() => s.node_drag = None,
            Phase::Up => s.drop_node(),
        });
    if open {
        node = node.on_double_press(move |s: &mut App| s.enter(enter.clone()));
    }
    // Connectors sit on the node's edges, half outside it, so they're siblings drawn after it rather than children
    // its rounded clip would cut.
    let mut group = el().key(format!("group:{}", op.path)).absolute(0.0, 0.0).child(node);
    let slots = s.input_slots(op);
    for (i, wired) in slots.iter().enumerate() {
        let c = geometry::input_point(pos, i, slots.len());
        let to = op.path.clone();
        group = group.child(
            connector(k, c, *wired, color)
                .key(format!("in:{}:{i}", op.path))
                .on_drag(move |s: &mut App, d: &mut Drag, _: &mut Tx| match d.phase {
                    Phase::Down => s.pick_up_wire(&to, i),
                    Phase::Move => s.move_wire([d.dx, d.dy]),
                    Phase::Up if d.cancelled() => s.wire_drag = None,
                    Phase::Up => s.drop_wire(),
                }),
        );
    }
    if s.has_output(op) {
        let from = op.path.clone();
        group = group.child(
            connector(k, geometry::output_point(pos), true, color)
                .key(format!("out:{}", op.path))
                .on_drag(move |s: &mut App, d: &mut Drag, _: &mut Tx| match d.phase {
                    Phase::Down => s.start_wire(&from),
                    Phase::Move => s.move_wire([d.dx, d.dy]),
                    Phase::Up if d.cancelled() => s.wire_drag = None,
                    Phase::Up => s.drop_wire(),
                }),
        );
    }
    group
}

fn connector(k: &Kit, centre: [f32; 2], filled: bool, color: Color) -> El {
    el().absolute(centre[0] - CONNECTOR / 2.0, centre[1] - CONNECTOR / 2.0)
        .size(CONNECTOR, CONNECTOR)
        .radius(CONNECTOR / 2.0)
        .fill(if filled { color } else { k.c.layer_02 })
        .border(1.5, color)
        .hover(k.c.hover)
}

/// The preview over a node's title: its live picture or values, an error, or nothing when zoomed out.
fn preview(s: &App, k: &Kit, op: &Op) -> El {
    let area = col()
        .key("preview")
        .w_full()
        .h(PREVIEW_H)
        .padding(6.0)
        .gap(2.0)
        .fill(k.c.bg_deep);
    if !s.camera.shows_previews() {
        return area;
    }
    if let Some(error) = &op.error {
        return area.child(text(error.clone()).font(SMALL).color(k.c.danger.fg).wrap().max_lines(3));
    }
    if op.family == Family::Component {
        let count = s
            .snapshot
            .as_ref()
            .map_or(0, |snapshot| network::children(snapshot, &op.path).count());
        return area.center().child(
            text(format!("{count} operator{}", if count == 1 { "" } else { "s" }))
                .font(SMALL)
                .color(k.c.text_faint),
        );
    }
    match s.previews.get(&op.path) {
        Some(Preview::Channel(values)) => {
            let shown = values.len().min(if values.len() > 3 { 2 } else { 3 });
            let mut area = area.justify_center();
            for (name, value) in &values[..shown] {
                area = area.child(channel_row(k, name, *value, family_color(op.family)));
            }
            if values.len() > shown {
                area = area.child(
                    text(format!("+{} more", values.len() - shown))
                        .font(SMALL)
                        .color(k.c.text_faint),
                );
            }
            area
        }
        Some(Preview::Texture(rect)) => match s.previews.atlas() {
            Some(atlas) => {
                let (picture, whole) = geometry::atlas_crop(rect, atlas.size, [NODE_W, PREVIEW_H]);
                area.padding(0.0).child(
                    el().key("picture")
                        .absolute(picture[0], picture[1])
                        .size(picture[2], picture[3])
                        .clip_soft(0.0)
                        .child(
                            el().absolute(whole[0], whole[1])
                                .size(whole[2], whole[3])
                                .native_surface(&atlas.surface, SurfaceFit::Fill),
                        ),
                )
            }
            None => area,
        },
        Some(Preview::Pending) => area
            .center()
            .linear_gradient(2.4, [(0.0, family_color(op.family).alpha(0.25)), (1.0, k.c.bg_deep)]),
        Some(Preview::Error(message)) => area.child(text(message).font(SMALL).color(k.c.danger.fg).wrap().max_lines(3)),
        None => area,
    }
}

/// A channel's name and value over a thin bar from the middle (zero) toward ±1.
fn channel_row(k: &Kit, name: &str, value: f32, color: Color) -> El {
    const BAR_W: f32 = NODE_W - 12.0;
    let v = value.clamp(-1.0, 1.0);
    let half = BAR_W / 2.0;
    let (left, width) = if v >= 0.0 {
        (half, v * half)
    } else {
        (half + v * half, -v * half)
    };
    col()
        .w_full()
        .child(
            row()
                .w_full()
                .h(12.0)
                .gap(4.0)
                .center_y()
                .child(
                    text(name.to_string())
                        .font(SMALL)
                        .color(k.c.text_muted)
                        .truncate()
                        .grow(1.0)
                        .min_w(0.0),
                )
                .child(text(format!("{value:.3}")).font(super::MONO).color(k.c.text_base)),
        )
        .child(
            el().w(BAR_W)
                .h(2.0)
                .fill(k.c.layer_03)
                .child(el().absolute(left, 0.0).size(width.max(1.0), 2.0).fill(color)),
        )
}
