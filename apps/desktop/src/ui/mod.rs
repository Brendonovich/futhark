//! The window: a toolbar, the network editor, and a side panel with the viewer over the inspector.

mod editor;
mod inspector;
mod viewer;

use std::path::PathBuf;

use futhark_protocol::Family;
use rune::{Color, Cx, El, FileDialogOptions, TextStyle, Tx, col, el, oklch, row, spacer, text};
use rune_kit::{Button, ButtonVariant, Kit, Menu, MenuItem, TextInput};

use crate::app::{App, Connection};
use crate::network;

pub const SIDE_W: f32 = 320.0;
const TOOLBAR_H: f32 = 40.0;
pub const SMALL: TextStyle = TextStyle::new(11.0, 400.0);
pub const SMALL_MEDIUM: TextStyle = TextStyle::new(11.0, 500.0);
pub const MONO: TextStyle = TextStyle::new(11.0, 400.0).mono().features("'tnum' 1");

pub fn kit(dark: bool) -> Kit {
    rune_theme_opencode::kit(dark).clone()
}

/// Each family's colour, for node title bars and wires.
pub fn family_color(family: Family) -> Color {
    let hue = match family {
        Family::Texture => 300.0,
        Family::Channel => 145.0,
        Family::Geometry => 240.0,
        Family::Data => 350.0,
        Family::Shader => 85.0,
        Family::Component => 0.0,
    };
    let chroma = if family == Family::Component { 0.0 } else { 0.13 };
    oklch(0.68, chroma, hue)
}

pub fn view(s: &App, cx: &mut Cx<App>) -> El {
    s.scale.set(cx.env().scale);
    let k = kit(cx.system_dark());
    let mut layer = Vec::new();
    let body = match &s.snapshot {
        Some(_) => row()
            .key("body")
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(editor::editor(s, &k))
            .child(side(s, &k, &mut layer)),
        None => no_project(s, &k),
    };
    let window = col()
        .w_full()
        .h_full()
        .fill(k.c.bg_base)
        .child(toolbar(s, &k, &mut layer))
        .child(body);
    rune_kit::root(window, layer)
}

fn toolbar(s: &App, k: &Kit, layer: &mut Vec<El>) -> El {
    let mut bar = row()
        .key("toolbar")
        .w_full()
        .h(TOOLBAR_H)
        .px(12.0)
        .gap(4.0)
        .center_y()
        .fill(k.c.bg_deep)
        .border(0.0, k.c.border_muted);
    if let Some(snapshot) = &s.snapshot {
        bar = bar.child(
            text(snapshot.project.name.clone())
                .font(k.ty.ui_medium)
                .color(k.c.text_base)
                .mr(8.0),
        );
        let crumbs = network::breadcrumb(&s.component);
        let last = crumbs.len() - 1;
        for (i, (label, path)) in crumbs.into_iter().enumerate() {
            if i > 0 {
                bar = bar.child(text("/").font(k.ty.ui).color(k.c.text_faint));
            }
            let crumb = Button::new(label)
                .variant(if i == last {
                    ButtonVariant::Neutral
                } else {
                    ButtonVariant::Ghost
                })
                .on_press(move |s: &mut App| s.enter(path.clone()))
                .build(k)
                .key(format!("crumb:{i}"));
            bar = bar.child(crumb);
        }
    }
    bar = bar.child(spacer());
    if let Some(notice) = &s.notice {
        bar = bar.child(
            text(notice.clone())
                .key("notice")
                .font(k.ty.small)
                .color(k.c.text_muted)
                .truncate()
                .max_w(420.0)
                .mr(8.0),
        );
    }
    bar = bar.child(
        text(status(s))
            .key("status")
            .font(k.ty.small)
            .color(k.c.text_faint)
            .mr(8.0),
    );
    if s.snapshot.is_some() {
        bar = bar
            .child(add_menu(s, k, layer))
            .child(
                Button::new("Save")
                    .variant(ButtonVariant::Ghost)
                    .on_press(|s: &mut App| s.save())
                    .build(k)
                    .key("save"),
            )
            .child(open_button(k));
    }
    bar
}

fn status(s: &App) -> String {
    match &s.connection {
        Connection::Connecting => "Connecting to futharkd…".into(),
        Connection::Connected { hello, .. } => format!("futharkd {}", hello.version),
        Connection::Offline => "Offline".into(),
        Connection::Lost(why) => format!("Disconnected: {why}"),
    }
}

fn open_button(k: &Kit) -> El {
    Button::new("Open…")
        .variant(ButtonVariant::Ghost)
        .on_press_tx(|_: &mut App, tx: &mut Tx| choose_project(tx))
        .build(k)
        .key("open")
}

fn choose_project(tx: &mut Tx) {
    let options = FileDialogOptions {
        title: "Open a Futhark project".into(),
        ..Default::default()
    };
    tx.open_folder(options, |s: &mut App, result, _| match result {
        Ok(Some(path)) => s.open_project(path),
        Ok(None) => {}
        Err(error) => s.notice = Some(format!("{error}; type the project's path instead")),
    });
}

/// The menu of operator types, grouped by family. Opens under its button, or where the canvas was right-clicked.
fn add_menu(s: &App, k: &Kit, layer: &mut Vec<El>) -> El {
    let trigger = Button::new("Add")
        .variant(ButtonVariant::Neutral)
        .shortcut("⇥")
        .on_press(|s: &mut App| s.open_add_menu(None, None))
        .build(k)
        .key("add");
    let mut menu = Menu::new("add-menu")
        .trigger(trigger)
        .open(s.add_menu.is_some())
        .min_width(200.0)
        .on_close(|s: &mut App| s.add_menu = None);
    let Some(add) = s.add_menu else {
        return menu.build(k, layer);
    };
    if let Some([x, y]) = add.at {
        menu = menu.at(x, y);
    }
    for family in network::FAMILIES {
        let mut types: Vec<_> = s.types.iter().filter(|t| t.family == family).collect();
        if types.is_empty() {
            continue;
        }
        types.sort_by(|a, b| a.label.cmp(&b.label));
        menu = menu.group_label(network::family_label(family));
        for t in types {
            let name = t.name.clone();
            menu = menu.item(MenuItem::new(t.label.clone()).on_select(move |s: &mut App| s.add_op(&name, add.world)));
        }
    }
    menu.build(k, layer)
}

fn side(s: &App, k: &Kit, layer: &mut Vec<El>) -> El {
    col()
        .key("side")
        .w(SIDE_W)
        .h_full()
        .min_h(0.0)
        .fill(k.c.bg_deep)
        .child(viewer::viewer(s, k))
        .child(el().w_full().h(1.0).fill(k.c.border_muted))
        .child(inspector::inspector(s, k, layer))
}

fn no_project(s: &App, k: &Kit) -> El {
    let field = TextInput::new(&s.open_path)
        .placeholder("/path/to/project")
        .width(Some(320.0))
        .on_edit(|s: &mut App| &mut s.open_path)
        .on_submit(|s: &mut App, _| open_typed(s))
        .build(k)
        .key("open-path");
    let message = match &s.connection {
        Connection::Connecting => "Connecting to futharkd…".to_string(),
        Connection::Lost(why) => format!("Can't reach futharkd: {why}"),
        _ => "No project is open.".to_string(),
    };
    let mut panel = col()
        .key("no-project")
        .gap(12.0)
        .items_center()
        .child(text("Futhark").font(TextStyle::new(22.0, 600.0)).color(k.c.text_base))
        .child(text(message).font(k.ty.ui).color(k.c.text_muted));
    if matches!(s.connection, Connection::Connected { .. } | Connection::Offline) {
        panel = panel
            .child(
                Button::new("Open project…")
                    .variant(ButtonVariant::Contrast)
                    .on_press_tx(|_: &mut App, tx: &mut Tx| choose_project(tx))
                    .build(k)
                    .key("choose-project"),
            )
            .child(
                row()
                    .gap(8.0)
                    .center_y()
                    .child(field)
                    .child(Button::new("Open").on_press(open_typed).build(k).key("open-typed")),
            );
    }
    if let Some(notice) = &s.notice {
        panel = panel.child(text(notice.clone()).font(k.ty.small).color(k.c.danger.fg));
    }
    col().w_full().grow(1.0).center().child(panel)
}

fn open_typed(s: &mut App) {
    let path = s.open_path.text.trim().to_string();
    if !path.is_empty() {
        s.open_project(PathBuf::from(path));
    }
}
