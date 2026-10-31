//! The whole view, headless: a fixed network on a runtime with no window, GPU or server, driven by pointer and keys.
//! Edits land in the app's outbox.

use futhark_protocol::{Edit, Value};
use rune::{Env, Key, KeyEvent, Mods, Runtime, WheelEvent};

use crate::app::App;
use crate::fixtures;
use crate::geometry::{self, NODE_H, NODE_W};
use crate::session::Command;
use crate::ui;

const SIZE: (f32, f32) = (1200.0, 800.0);

fn runtime() -> Runtime<App> {
    let app = App::offline(fixtures::types(), fixtures::snapshot());
    let mut rt = Runtime::new(app, ui::view, SIZE, Env::new(true, 2.0));
    settle(&mut rt);
    rt
}

fn settle(rt: &mut Runtime<App>) {
    rt.frame(0.0);
    for _ in 0..120 {
        if !rt.frame(1.0 / 60.0) && !rt.has_pending_input() {
            return;
        }
    }
}

fn rect(rt: &Runtime<App>, key: &str) -> [f32; 4] {
    rt.elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .unwrap_or_else(|| panic!("no element {key}"))
        .rect
}

fn has(rt: &Runtime<App>, key: &str) -> bool {
    rt.elements().iter().any(|e| e.key.as_deref() == Some(key))
}

fn centre(r: [f32; 4]) -> (f32, f32) {
    (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
}

fn drag(rt: &mut Runtime<App>, from: (f32, f32), to: (f32, f32)) {
    rt.pointer_move(from.0, from.1);
    rt.pointer_down(from.0, from.1);
    for i in 1..=4 {
        let t = i as f32 / 4.0;
        rt.pointer_move(from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
        rt.frame(1.0 / 60.0);
    }
    rt.pointer_up(to.0, to.1);
    settle(rt);
}

fn click(rt: &mut Runtime<App>, at: (f32, f32)) {
    rt.pointer_move(at.0, at.1);
    rt.pointer_down(at.0, at.1);
    rt.pointer_up(at.0, at.1);
    settle(rt);
}

fn edits(rt: &mut Runtime<App>) -> Vec<Edit> {
    std::mem::take(&mut rt.state.outbox)
        .into_iter()
        .flat_map(|c| match c {
            Command::Edit { base, edits } => {
                assert_eq!(base, 3, "edits go against the snapshot's revision");
                edits
            }
            other => panic!("expected an edit, got {other:?}"),
        })
        .collect()
}

#[test]
fn the_editor_shows_the_components_nodes_and_wires() {
    let rt = runtime();
    for path in ["/fx", "/lfo1", "/mix", "/noise1", "/ramp1"] {
        assert!(has(&rt, &format!("node:{path}")), "{path} shows");
    }
    assert!(!has(&rt, "node:/fx/wobble"), "operators inside /fx stay inside it");
    assert!(has(&rt, "wires"));
    // mix takes any number of inputs: its two wires and a free slot.
    assert!(has(&rt, "in:/mix:2") && !has(&rt, "in:/mix:3"));
    assert!(
        has(&rt, "out:/lfo1") && !has(&rt, "out:/fx"),
        "components have no output"
    );
    // The camera framed the network: its nodes are inside the canvas.
    let canvas = rect(&rt, "canvas");
    let lfo = rect(&rt, "node:/lfo1");
    assert!(lfo[0] > canvas[0] && lfo[1] > canvas[1] && lfo[0] + lfo[2] < canvas[0] + canvas[2]);
    assert!((lfo[2] - NODE_W * rt.state.camera.zoom).abs() < 0.5);
}

#[test]
fn previews_follow_what_shows() {
    let mut rt = runtime();
    let visible: Vec<String> = rt.state.visible_previews().iter().map(|r| r.path.clone()).collect();
    assert_eq!(
        visible,
        ["/lfo1", "/mix", "/noise1", "/ramp1"],
        "every node but the component"
    );
    let zoom = rt.state.camera.zoom;
    assert_eq!(
        rt.state.visible_previews()[0].pixels,
        rt.state.camera.preview_pixels(2.0)
    );
    assert!(zoom > 0.9);

    // Pan the network out of view: nothing to preview.
    let (x, y) = centre(rect(&rt, "canvas"));
    rt.wheel_event(WheelEvent::new(-5000.0, 0.0).at(x, y).precise(true));
    settle(&mut rt);
    assert!(rt.state.visible_previews().is_empty());
    rt.wheel_event(WheelEvent::new(5000.0, 0.0).at(x, y).precise(true));
    settle(&mut rt);
    assert_eq!(rt.state.visible_previews().len(), 4);

    // Zoomed far out, previews stop.
    let mods = Mods {
        cmd: true,
        ..Mods::default()
    };
    for _ in 0..4 {
        rt.wheel_event(WheelEvent::new(0.0, 80.0).at(x, y).mods(mods));
        settle(&mut rt);
    }
    assert!(!rt.state.camera.shows_previews(), "zoom {}", rt.state.camera.zoom);
    assert!(rt.state.visible_previews().is_empty());
}

#[test]
fn clicking_selects_and_the_inspector_follows_the_schema() {
    let mut rt = runtime();
    assert!(has(&rt, "inspector") && !has(&rt, "param:frequency"));
    let at = centre(rect(&rt, "node:/lfo1"));
    click(&mut rt, at);
    assert_eq!(rt.state.selected.as_deref(), Some("/lfo1"));
    for param in ["wave", "frequency", "name"] {
        assert!(has(&rt, &format!("param:{param}")), "{param} row");
    }
    assert!(has(&rt, "number:frequency") && has(&rt, "field:name") && has(&rt, "menu:wave-trigger"));
    assert!(edits(&mut rt).is_empty(), "selecting changes nothing on the server");

    // Dragging the number sets it, from the snapshot's 0.5, 200 points across its 0..10 soft range.
    let (x, y) = centre(rect(&rt, "number:frequency"));
    drag(&mut rt, (x, y), (x + 20.0, y));
    let sets = edits(&mut rt);
    let Some(Edit::Set { path, param, value }) = sets.last() else {
        panic!("expected a Set, got {sets:?}")
    };
    assert_eq!((path.as_str(), param.as_str()), ("/lfo1", "frequency"));
    assert_eq!(*value, Value::Float(1.5));

    // A click on the background deselects.
    let canvas = rect(&rt, "canvas");
    click(&mut rt, (canvas[0] + canvas[2] - 20.0, canvas[1] + canvas[3] - 20.0));
    assert_eq!(rt.state.selected, None);
}

#[test]
fn dragging_a_node_moves_it_on_release() {
    let mut rt = runtime();
    let zoom = rt.state.camera.zoom;
    let (x, y) = centre(rect(&rt, "node:/ramp1"));
    drag(&mut rt, (x, y), (x + 40.0, y + 20.0));
    let moved = edits(&mut rt);
    let expected = Edit::Move {
        path: "/ramp1".into(),
        x: (160.0 + 40.0 / zoom).round(),
        y: (160.0 + 20.0 / zoom).round(),
    };
    assert_eq!(moved, [expected]);
    // It stays where it was dropped while the server catches up.
    let after = rect(&rt, "node:/ramp1");
    assert!((centre(after).0 - (x + 40.0)).abs() < 1.5, "{after:?}");
}

#[test]
fn wires_connect_and_disconnect() {
    let mut rt = runtime();
    // Out of lfo1 into mix's free input.
    let (from, to) = (centre(rect(&rt, "out:/lfo1")), centre(rect(&rt, "in:/mix:2")));
    drag(&mut rt, from, to);
    assert_eq!(
        edits(&mut rt),
        [Edit::Connect {
            from: "/lfo1".into(),
            to: "/mix".into(),
            input: 2
        }]
    );
    // Dropped on a node's body: its first free input.
    let (from, to) = (centre(rect(&rt, "out:/lfo1")), centre(rect(&rt, "node:/ramp1")));
    drag(&mut rt, from, to);
    assert_eq!(
        edits(&mut rt),
        [Edit::Connect {
            from: "/lfo1".into(),
            to: "/ramp1".into(),
            input: 0
        }]
    );
    // Picked up off mix's second input and dropped on nothing.
    let canvas = rect(&rt, "canvas");
    let from = centre(rect(&rt, "in:/mix:1"));
    drag(
        &mut rt,
        from,
        (canvas[0] + canvas[2] - 30.0, canvas[1] + canvas[3] - 30.0),
    );
    assert_eq!(
        edits(&mut rt),
        [Edit::Disconnect {
            to: "/mix".into(),
            input: 1
        }]
    );
    assert!(rt.state.wire_drag.is_none());
}

#[test]
fn components_open_with_a_double_click_and_close_from_the_breadcrumb() {
    let mut rt = runtime();
    let at = centre(rect(&rt, "node:/fx"));
    rt.pointer_move(at.0, at.1);
    rt.pointer_down(at.0, at.1);
    rt.pointer_up(at.0, at.1);
    rt.frame(0.05);
    rt.pointer_down(at.0, at.1);
    rt.pointer_up(at.0, at.1);
    settle(&mut rt);
    assert_eq!(rt.state.component, "/fx");
    assert!(has(&rt, "node:/fx/wobble") && !has(&rt, "node:/lfo1"));
    assert!(has(&rt, "crumb:1"));
    let at = centre(rect(&rt, "crumb:0"));
    click(&mut rt, at);
    assert_eq!(rt.state.component, "/");
    assert!(has(&rt, "node:/lfo1"));
}

#[test]
fn keys_delete_and_open_the_add_menu() {
    let mut rt = runtime();
    rt.on_key = Some(crate::on_key);
    let at = centre(rect(&rt, "node:/noise1"));
    click(&mut rt, at);
    rt.key(KeyEvent {
        key: Key::Backspace,
        mods: Mods::default(),
    });
    settle(&mut rt);
    assert_eq!(edits(&mut rt), [Edit::Remove { path: "/noise1".into() }]);

    rt.key(KeyEvent {
        key: Key::Tab,
        mods: Mods::default(),
    });
    settle(&mut rt);
    assert!(rt.state.add_menu.is_some());
    assert!(has(&rt, "add-menu-menu"), "the menu opened");
    let item = rt
        .elements()
        .into_iter()
        .find(|e| e.label.as_deref() == Some("lfo") && e.role == "menuitem")
        .expect("an LFO item");
    click(&mut rt, centre(item.rect));
    let added = edits(&mut rt);
    assert!(
        matches!(&added[..], [Edit::Add { path, ty }, Edit::Move { .. }] if path == "/lfo2" && ty == "channel/lfo"),
        "{added:?}"
    );
    assert!(rt.state.add_menu.is_none());
}

#[test]
fn new_nodes_land_where_the_canvas_was_clicked() {
    let mut app = App::offline(fixtures::types(), fixtures::snapshot());
    app.set_viewport([800.0, 600.0]);
    let camera = app.camera;
    app.open_add_menu(Some([10.0, 10.0]), Some([300.0, 200.0]));
    let world = camera.to_world([300.0, 200.0]);
    let menu = app.add_menu.expect("open");
    assert_eq!(menu.world, [world[0] - NODE_W / 2.0, world[1] - NODE_H / 2.0]);
    assert!(geometry::contains(geometry::node_rect(menu.world), world));
}
