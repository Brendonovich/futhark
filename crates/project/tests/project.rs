use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use futhark_graph::{Clock, Engine, Registry, Value};
use futhark_project::{Diagnostic, Project, load, save};

fn registry() -> Arc<Registry> {
    Arc::new(futhark_ops::registry())
}

fn example(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples").join(name)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("futhark-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Every file under `dir`, relative to it, with its contents.
fn files(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push((
                    path.strip_prefix(dir).unwrap().into(),
                    fs::read_to_string(&path).unwrap(),
                ));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn examples_load_cleanly_and_save_unchanged() {
    for entry in fs::read_dir(example("")).unwrap() {
        let dir = entry.unwrap().path();
        let (project, diagnostics) = load(&dir, registry()).unwrap();
        assert_eq!(diagnostics, [], "{}", dir.display());

        let out = scratch(&format!("roundtrip-{}", dir.file_name().unwrap().to_string_lossy()));
        save(&out, &project).unwrap();
        assert_eq!(files(&out), files(&dir), "{} doesn't round-trip", dir.display());
    }
}

#[test]
fn the_hello_example_cooks() {
    let (project, _) = load(&example("hello"), registry()).unwrap();
    assert_eq!(project.name, "hello");
    let mut engine = Engine::new(
        project.network,
        Clock {
            rate: project.rate,
            frame: 30,
        },
    );
    let mix = engine.network().find("/mix").unwrap();
    let output = engine.pull(mix).unwrap();
    let channels = output.channels().unwrap();
    assert_eq!(channels.channels()[0].name, "chan1");

    let wobble = engine.network().find("/fx/wobble").unwrap();
    assert_eq!(
        engine.network().param(wobble, "wave"),
        Some(&Value::Menu("triangle".into()))
    );
    assert!(engine.pull(wobble).unwrap().channels().unwrap().get("wobble").is_some());
}

#[test]
fn saving_writes_the_house_style() {
    let mut project = Project::new("show \"one\"", registry());
    let net = &mut project.network;
    let root = net.root();
    let b = net.add(root, "b", "channel/math").unwrap();
    let a = net.add(root, "a", "channel/constant").unwrap();
    let c = net.add(root, "c", "channel/constant").unwrap();
    net.set_param(a, "value", Value::Int(2)).unwrap();
    net.set_param(a, "name", Value::String("x\ty".into())).unwrap();
    net.set_param(b, "combine", Value::Menu("maximum".into())).unwrap();
    net.set_param(b, "gain", Value::Float(1.0)).unwrap();
    net.connect(c, b, 2).unwrap();
    net.set_position(b, [10.5, -3.0]);

    let dir = scratch("style");
    save(&dir, &project).unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("project.kdl")).unwrap(),
        "name \"show \\\"one\\\"\"\nrate 60.0\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("root/component.kdl")).unwrap(),
        "a \"channel/constant\" {\n    name \"x\\ty\"\n    value 2.0\n}\n\n\
         b \"channel/math\" {\n    input #null\n    input #null\n    input c\n    combine maximum\n}\n\n\
         c \"channel/constant\"\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("root/layout.kdl")).unwrap(),
        "a 0.0 0.0\nb 10.5 -3.0\nc 0.0 0.0\n"
    );

    let (loaded, diagnostics) = load(&dir, registry()).unwrap();
    assert_eq!(diagnostics, []);
    let b = loaded.network.find("/b").unwrap();
    assert_eq!(loaded.network.node(b).inputs(), [None, None, loaded.network.find("/c")]);
}

#[test]
fn broken_files_load_what_they_can() {
    let dir = scratch("broken");
    fs::create_dir_all(dir.join("root/fx")).unwrap();
    fs::write(dir.join("project.kdl"), "name \"broken\"\nrate 30\n").unwrap();
    fs::write(
        dir.join("root/component.kdl"),
        r#"
        good "channel/math" {
            input gone
            input lfo
            gain "loud"
            volume 3
        }
        lfo "channel/lfo"
        odd "channel/teleport"
        fx "component/base"
        "#,
    )
    .unwrap();
    fs::write(dir.join("root/fx/component.kdl"), "this is { not kdl").unwrap();

    let (project, diagnostics) = load(&dir, registry()).unwrap();
    assert_eq!(project.rate, 30.0);
    let messages: Vec<String> = diagnostics.iter().map(Diagnostic::to_string).collect();
    assert_eq!(messages.len(), 5, "{messages:#?}");
    assert!(
        messages[0].starts_with("/good: gain: expected a float"),
        "{}",
        messages[0]
    );
    assert_eq!(messages[1], "/good: no parameter \"volume\"");
    assert_eq!(messages[2], "/odd: unknown operator type \"channel/teleport\"");
    assert_eq!(messages[3], "/good: input 0: no operator named gone");
    assert!(messages[4].starts_with("root/fx/component.kdl: "), "{}", messages[4]);

    let good = project.network.find("/good").unwrap();
    assert_eq!(
        project.network.node(good).inputs(),
        [None, project.network.find("/lfo")]
    );
}
