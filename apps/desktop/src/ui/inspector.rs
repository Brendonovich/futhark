//! The parameter inspector for the selected operator, built from its type's schema.

use futhark_protocol::{Op, ParamKind, ParamType, Value};
use rune::{Drag, El, Key, KeyEvent, Phase, ScrollSpec, Tx, col, el, row, text};
use rune_kit::{Kit, Select, Switch, TextInput};

use super::{MONO, SMALL, SMALL_MEDIUM};
use crate::app::{self, App};
use crate::network;

const LABEL_W: f32 = 104.0;
const ROW_H: f32 = 26.0;

pub fn inspector(s: &App, k: &Kit, layer: &mut Vec<El>) -> El {
    let panel = col()
        .key("inspector")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .scroll(ScrollSpec { stick_bottom: false });
    let Some(op) = s.selected.as_deref().and_then(|path| s.op(path)) else {
        return panel.child(
            col()
                .padding(12.0)
                .child(text("Nothing selected").font(k.ty.small).color(k.c.text_faint)),
        );
    };
    let ty = s.op_type(op);
    let mut body = col().w_full().padding(12.0).gap(4.0).child(
        row()
            .gap(6.0)
            .center_y()
            .mb(4.0)
            .child(
                text(network::name(&op.path).to_string())
                    .font(k.ty.ui_medium)
                    .color(k.c.text_base),
            )
            .child(
                text(ty.map_or(op.ty.clone(), |t| t.label.clone()))
                    .font(k.ty.small)
                    .color(k.c.text_faint),
            ),
    );
    let Some(ty) = ty else {
        return panel.child(
            body.child(
                text(format!("Unknown operator type {}", op.ty))
                    .font(k.ty.small)
                    .color(k.c.danger.fg),
            ),
        );
    };
    if !ty.doc.is_empty() {
        body = body.child(text(ty.doc.clone()).font(SMALL).color(k.c.text_muted).wrap().mb(6.0));
    }
    if ty.params.is_empty() {
        body = body.child(text("No parameters").font(SMALL).color(k.c.text_faint));
    }
    for param in &ty.params {
        body = body.child(param_row(s, k, op, param, layer));
    }
    panel.child(body)
}

fn param_row(s: &App, k: &Kit, op: &Op, ty: &ParamType, layer: &mut Vec<El>) -> El {
    let value = s.param_value(op, &ty.name).unwrap_or_else(|| ty.default.clone());
    let control = match &ty.kind {
        ParamKind::Float | ParamKind::Int => number(s, k, ty, &value),
        ParamKind::Toggle => {
            let on = value == Value::Bool(true);
            let name = ty.name.clone();
            Switch::new(on)
                .hide_label()
                .label(ty.label.clone())
                .on_toggle(move |s: &mut App| s.set_param(&name, Value::Bool(!on)))
                .build(k)
        }
        ParamKind::Menu { items } => {
            let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
            let current = items.iter().find(|i| Value::String(i.name.clone()) == value);
            let (toggle, choose) = (ty.name.clone(), ty.name.clone());
            let items = items.clone();
            Select::new(format!("menu:{}", ty.name), &labels)
                .width(180.0)
                .current(current.map(|i| i.label.as_str()))
                .open(s.open_select.as_deref() == Some(ty.name.as_str()))
                .on_toggle(move |s: &mut App| {
                    s.open_select = if s.open_select.as_deref() == Some(toggle.as_str()) {
                        None
                    } else {
                        Some(toggle.clone())
                    };
                })
                .on_close(|s: &mut App| s.open_select = None)
                .on_choose(move |s: &mut App, label: &str, _: &mut Tx| {
                    s.open_select = None;
                    if let Some(item) = items.iter().find(|i| i.label == label) {
                        s.set_param(&choose, Value::String(item.name.clone()));
                    }
                })
                .build(k, layer)
        }
        ParamKind::String => field(s, k, ty, value.to_string()),
    };
    row()
        .key(format!("param:{}", ty.name))
        .w_full()
        .min_h(ROW_H)
        .gap(8.0)
        .center_y()
        .help_when_truncated(ty.doc.clone())
        .child(
            text(ty.label.clone())
                .font(SMALL_MEDIUM)
                .color(k.c.text_muted)
                .w(LABEL_W)
                .truncate(),
        )
        .child(row().grow(1.0).min_w(0.0).child(control))
}

/// A text field for a parameter: always for strings, and for numbers after a double-click.
fn field(s: &App, k: &Kit, ty: &ParamType, current: String) -> El {
    let shown = s
        .fields
        .get(&ty.name)
        .cloned()
        .unwrap_or_else(|| rune::TextEdit::new(current.clone()));
    let (edit, submit, escape) = (ty.name.clone(), ty.clone(), ty.name.clone());
    TextInput::new(&shown)
        .width(None)
        .height(ROW_H - 2.0)
        .text_style(MONO)
        .on_edit(move |s: &mut App| {
            if !s.fields.contains_key(&edit) {
                s.fields.insert(edit.clone(), rune::TextEdit::new(current.clone()));
            }
            s.field(&edit)
        })
        .on_submit(move |s: &mut App, _| s.submit_field(&submit))
        .on_key(move |s: &mut App, e: &KeyEvent, _| {
            if e.key == Key::Escape {
                s.fields.remove(&escape);
                return true;
            }
            false
        })
        .build(k)
        .key(format!("field:{}", ty.name))
}

/// A number: drag sideways to change it (Shift for fine steps), double-click to type.
fn number(s: &App, k: &Kit, ty: &ParamType, value: &Value) -> El {
    if s.fields.contains_key(&ty.name) {
        return field(s, k, ty, value.to_string());
    }
    let current = app::number(value).unwrap_or(0.0);
    let lo = ty.soft_min.or(ty.min);
    let hi = ty.soft_max.or(ty.max);
    let fraction = match (lo, hi) {
        (Some(lo), Some(hi)) if hi > lo => ((current - lo) / (hi - lo)).clamp(0.0, 1.0) as f32,
        _ => 0.0,
    };
    let shown = match value {
        Value::Float(v) => format!("{v:.3}"),
        other => other.to_string(),
    };
    let (drag, typed) = (ty.clone(), ty.name.clone());
    let typed_text = value.to_string();
    el().key(format!("number:{}", ty.name))
        .w_full()
        .h(ROW_H - 2.0)
        .radius(k.radius.sm)
        .fill(k.c.layer_03)
        .hover(k.c.hover)
        .clip_rounded(k.radius.sm)
        .cursor(rune::Cursor::ResizeColumn)
        .child(el().absolute(0.0, 0.0).w_frac(fraction).h_full().fill(k.c.layer_04))
        .child(
            row()
                .w_full()
                .h_full()
                .px(8.0)
                .center_y()
                .child(text(shown).font(MONO).color(k.c.text_base)),
        )
        .on_drag(move |s: &mut App, d: &mut Drag, tx: &mut Tx| match d.phase {
            Phase::Down => {}
            Phase::Move => s.drag_param(&drag, current, d.dx, tx.mods().shift),
            Phase::Up => s.end_param_drag(),
        })
        .on_double_press(move |s: &mut App| s.edit_field(&typed, typed_text.clone()))
}
