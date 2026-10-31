//! The Futhark desktop app: a client of `futharkd` with a network editor, viewers and a parameter inspector.
//!
//! `cargo run -p futhark-desktop -- [project]` opens the window, starting the server if it isn't running and opening
//! `project` if given. Headless: `-- --snap out.png --scene editor` renders a fixed network without a server.

mod app;
mod fixtures;
mod frames;
mod geometry;
mod network;
mod previews;
mod session;
mod ui;
mod viewer;

#[cfg(test)]
mod server_tests;
#[cfg(test)]
mod tests;

use std::path::PathBuf;

use rune::{AppSpec, Key, KeyEvent, Tx};

use crate::app::{App, Connection};
use crate::previews::StaticPreviews;

fn init() -> App {
    App::new(Box::new(StaticPreviews::default()))
}

/// The project named on the command line, if any: the first argument Rune didn't take.
fn project_arg() -> Option<PathBuf> {
    rune::extra_args()
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(PathBuf::from)
}

fn setup(s: &mut App, wake: rune::Waker) {
    // Scenes run on fixtures, and headless runs never start a server.
    if matches!(s.connection, Connection::Offline) || rune::is_headless() {
        return;
    }
    s.connect(futhark_protocol::socket_path(), project_arg(), wake);
}

fn poll(s: &mut App, _clock: f64, _tx: &mut Tx) -> bool {
    s.poll()
}

fn on_key(s: &mut App, e: &KeyEvent, tx: &mut Tx) -> bool {
    if e.is_command('s') {
        s.save();
        return true;
    }
    if tx.focused().is_some() || s.snapshot.is_none() {
        return false;
    }
    match &e.key {
        Key::Backspace | Key::Delete => s.delete_selected(),
        Key::Tab => s.open_add_menu(None, None),
        Key::Escape if s.add_menu.is_some() => s.add_menu = None,
        Key::Escape if s.selected.is_some() => s.select(None),
        Key::Escape if s.component != "/" => {
            let up = network::parent(&s.component).to_string();
            s.enter(up);
        }
        _ => return false,
    }
    true
}

type Scene = (&'static str, fn(&mut App));

const SCENES: &[Scene] = &[
    ("editor", |s| *s = scenes::editor()),
    ("selected", |s| {
        *s = scenes::editor();
        s.select(Some("/lfo1".into()));
    }),
    ("no-project", |s| s.connection = Connection::Offline),
];

mod scenes {
    use std::collections::HashMap;

    use crate::app::App;
    use crate::fixtures;
    use rune::NativeSurface;
    use rune::native_surface::{ByteFrame, ColorInfo, Format};

    use crate::previews::{Atlas, Preview, StaticPreviews};

    /// A 256×64 atlas whose second 64×32 cell holds a horizontal ramp, and the rest dark red, so a scene shows
    /// whether a node crops its own cell.
    fn atlas() -> Atlas {
        let (w, h) = (256u32, 64u32);
        let mut bgra = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let inside = (64..128).contains(&x) && y < 32;
                let v = if inside { ((x - 64) * 4) as u8 } else { 0 };
                bgra.extend_from_slice(&if inside { [v, v, v, 255] } else { [0, 0, 120, 255] });
            }
        }
        let surface = NativeSurface::new();
        surface
            .push_bytes(
                ByteFrame {
                    width: w,
                    height: h,
                    format: Format::Bgra,
                    color: ColorInfo::default(),
                    planes: [bgra, Vec::new()],
                },
                1,
            )
            .expect("a valid frame");
        Atlas { surface, size: [w, h] }
    }

    /// The `hello` network with sample previews, and no server.
    pub fn editor() -> App {
        let mut app = App::offline(fixtures::types(), fixtures::snapshot());
        app.previews = Box::new(StaticPreviews {
            previews: HashMap::from([
                ("/lfo1".to_string(), Preview::Channel(vec![("chan1".into(), 0.71)])),
                ("/mix".to_string(), Preview::Channel(vec![("chan1".into(), -0.3)])),
                ("/ramp1".to_string(), Preview::Texture([64, 0, 64, 32])),
            ]),
            atlas: Some(atlas()),
        });
        app
    }
}

fn main() {
    if !rune::is_headless() {
        rune::prewarm_gpu();
    }
    rune::run(AppSpec {
        min_size: Some((900.0, 560.0)),
        scenes: SCENES,
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        fonts: rune_theme_opencode::FONTS,
        families: rune_theme_opencode::FAMILIES,
        ..AppSpec::new("Futhark", (1280.0, 800.0), init, ui::view)
    });
}
