//! How the engine cooks Texture operators: recooking, inherited resolution and format, and errors.

use std::sync::{Arc, OnceLock};

use futhark_graph::{Clock, Engine, Gpu, Network, OpId, Value};

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Arc::new(Gpu::headless().expect("a GPU adapter (on Linux CI, lavapipe)")))
        .clone()
}

fn engine() -> Engine {
    Engine::new(Network::new(Arc::new(futhark_ops::registry())), Clock::default()).with_gpu(gpu())
}

fn add(e: &mut Engine, name: &str, ty: &str) -> OpId {
    let net = e.network_mut();
    net.add(net.root(), name, ty).unwrap()
}

fn set(e: &mut Engine, id: OpId, param: &str, value: Value) {
    e.network_mut().set_param(id, param, value).unwrap();
}

/// The output's width, height and format.
fn shape(e: &mut Engine, id: OpId) -> (u32, u32, wgpu::TextureFormat) {
    let output = e.pull(id).unwrap();
    let texture = output.texture().unwrap();
    (texture.width(), texture.height(), texture.format())
}

fn pixel(e: &mut Engine, id: OpId) -> [u8; 4] {
    let output = e.pull(id).unwrap();
    let pixels = gpu().read_rgba8(output.texture().unwrap().texture()).unwrap();
    pixels[..4].try_into().unwrap()
}

#[test]
fn a_chain_recooks_only_on_change() {
    let mut e = engine();
    let constant = add(&mut e, "constant", "texture/constant");
    let level = add(&mut e, "level", "texture/level");
    let transform = add(&mut e, "transform", "texture/transform");
    e.network_mut().connect(constant, level, 0).unwrap();
    e.network_mut().connect(level, transform, 0).unwrap();

    let first = e.pull(transform).unwrap();
    for _ in 0..3 {
        e.advance();
        assert_eq!(
            e.pull(transform).unwrap(),
            first,
            "the same texture while nothing changes"
        );
    }
    assert_eq!([constant, level, transform].map(|id| e.cooks(id)), [1, 1, 1]);

    set(&mut e, level, "brightness", Value::Float(0.5));
    let [r, g, b, a] = pixel(&mut e, transform);
    assert!(
        [r, g, b].iter().all(|v| v.abs_diff(128) <= 1) && a == 255,
        "{:?}",
        [r, g, b, a]
    );
    assert_eq!([constant, level, transform].map(|id| e.cooks(id)), [1, 2, 2]);
}

#[test]
fn time_dependent_textures_reuse_pooled_textures() {
    let mut e = engine();
    let noise = add(&mut e, "noise", "texture/noise");
    let level = add(&mut e, "level", "texture/level");
    e.network_mut().connect(noise, level, 0).unwrap();
    let mut seen = Vec::new();
    for _ in 0..10 {
        e.advance();
        // The raw wgpu handle, so the pool is free to reuse the texture.
        let texture = e.pull(level).unwrap().texture().unwrap().texture().clone();
        if !seen.contains(&texture) {
            seen.push(texture);
        }
    }
    assert_eq!(e.cooks(level), 10);
    // The two operators share three textures: the two they hold, and one to draw into, freed as each replaces its
    // output.
    assert_eq!(seen.len(), 3);
}

#[test]
fn resolution_and_format_follow_the_first_input() {
    let mut e = engine();
    let constant = add(&mut e, "constant", "texture/constant");
    let level = add(&mut e, "level", "texture/level");
    e.network_mut().connect(constant, level, 0).unwrap();
    assert_eq!(shape(&mut e, constant), (256, 256, wgpu::TextureFormat::Rgba8Unorm));
    assert_eq!(shape(&mut e, level), (256, 256, wgpu::TextureFormat::Rgba8Unorm));

    set(&mut e, constant, "width", Value::Int(100));
    set(&mut e, constant, "height", Value::Int(50));
    set(&mut e, constant, "format", Value::Menu("rgba16f".into()));
    assert_eq!(shape(&mut e, level), (100, 50, wgpu::TextureFormat::Rgba16Float));

    set(&mut e, level, "width", Value::Int(32));
    set(&mut e, level, "format", Value::Menu("rgba32f".into()));
    assert_eq!(shape(&mut e, level), (32, 50, wgpu::TextureFormat::Rgba32Float));

    // 32-bit float can't be filtered, so it is sampled with a nearest sampler further down.
    let transform = add(&mut e, "transform", "texture/transform");
    e.network_mut().connect(level, transform, 0).unwrap();
    set(&mut e, transform, "format", Value::Menu("rgba8".into()));
    assert_eq!(shape(&mut e, transform), (32, 50, wgpu::TextureFormat::Rgba8Unorm));
    assert_eq!(pixel(&mut e, transform), [255; 4]);
}

#[test]
fn a_broken_shader_reports_an_error_and_recovers() {
    let mut e = engine();
    let shader = add(&mut e, "shader", "texture/shader");
    let level = add(&mut e, "level", "texture/level");
    e.network_mut().connect(shader, level, 0).unwrap();
    e.pull(level).unwrap();

    let broken = "@fragment\nfn main(in: Varyings) -> @location(0) vec4f {\n    return vec4f(nope);\n}\n";
    set(&mut e, shader, "code", Value::String(broken.into()));
    let error = e.pull(shader).unwrap_err().0;
    assert_eq!(error, "line 3:18: no definition in scope for identifier: `nope`");
    assert_eq!(e.pull(level).unwrap_err().0, "input shader has an error");
    // Still broken on later frames, without recompiling each time.
    e.advance();
    assert!(e.pull(level).is_err());

    // Valid WGSL that doesn't fit the operator fails when the pipeline is made.
    let no_main = "@fragment\nfn other() -> @location(0) vec4f {\n    return vec4f(1.0);\n}\n";
    set(&mut e, shader, "code", Value::String(no_main.into()));
    let error = e.pull(shader).unwrap_err().0;
    assert!(error.contains("entry point 'main'"), "{error}");

    let mistyped = "@fragment\nfn main(in: Varyings) -> @location(0) vec4f {\n    return 1.0;\n}\n";
    set(&mut e, shader, "code", Value::String(mistyped.into()));
    let error = e.pull(shader).unwrap_err().0;
    assert!(error.starts_with("line 3:12: "), "{error}");

    let fixed = "@fragment\nfn main(in: Varyings) -> @location(0) vec4f {\n    return vec4f(1.0, 0.0, 0.0, 1.0);\n}\n";
    set(&mut e, shader, "code", Value::String(fixed.into()));
    assert_eq!(pixel(&mut e, level), [255, 0, 0, 255]);
    assert!(e.error(shader).is_none());
}

#[test]
fn file_in_reports_missing_files() {
    let mut e = engine();
    let file = add(&mut e, "file", "texture/file_in");
    assert_eq!(e.pull(file).unwrap_err().0, "no file: set the path parameter");
    set(&mut e, file, "path", Value::String("missing.png".into()));
    assert!(e.pull(file).unwrap_err().0.starts_with("missing.png: "));
}

#[test]
fn textures_need_a_gpu() {
    let mut e = Engine::new(Network::new(Arc::new(futhark_ops::registry())), Clock::default());
    let constant = add(&mut e, "constant", "texture/constant");
    let error = e.pull(constant).unwrap_err().0;
    assert!(error.contains("need a GPU"), "{error}");
}
