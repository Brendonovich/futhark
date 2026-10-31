//! Golden-image tests for the Texture operators. Each renders small images and compares them with PNGs under
//! `tests/golden/`, allowing for small differences between GPUs (CI runs on lavapipe). `UPDATE_GOLDEN=1` rewrites the
//! references; on a mismatch the actual image is written next to the system temp dir for inspection.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use futhark_graph::{Clock, Engine, Gpu, Network, OpId, Value};

/// How far a channel may differ from the reference, out of 255.
const TOLERANCE: u8 = 3;
/// The fraction of pixels allowed past the tolerance, for pixels on edges that land differently on another GPU.
const OUTLIERS: f64 = 0.01;

fn gpu() -> Arc<Gpu> {
    static GPU: OnceLock<Arc<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| Arc::new(Gpu::headless().expect("a GPU adapter (on Linux CI, lavapipe)")))
        .clone()
}

fn engine() -> Engine {
    Engine::new(Network::new(Arc::new(futhark_ops::registry())), Clock::default()).with_gpu(gpu())
}

fn add(e: &mut Engine, name: &str, ty: &str, params: &[(&str, Value)]) -> OpId {
    let net = e.network_mut();
    let id = net.add(net.root(), name, ty).unwrap();
    for (param, value) in params {
        net.set_param(id, param, value.clone()).unwrap();
    }
    id
}

fn wire(e: &mut Engine, inputs: &[OpId], to: OpId) {
    for (index, input) in inputs.iter().enumerate() {
        e.network_mut().connect(*input, to, index).unwrap();
    }
}

fn f(v: f64) -> Value {
    Value::Float(v)
}

fn menu(v: &str) -> Value {
    Value::Menu(v.into())
}

/// A 64×64 generator.
fn sized(params: &[(&'static str, Value)]) -> Vec<(&'static str, Value)> {
    let mut out = vec![("width", Value::Int(64)), ("height", Value::Int(64))];
    out.extend_from_slice(params);
    out
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn write_png(path: &Path, width: u32, height: u32, pixels: &[u8]) {
    let file = fs::File::create(path).unwrap();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().unwrap().write_image_data(pixels).unwrap();
}

fn read_png(path: &Path) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(std::io::BufReader::new(fs::File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buffer).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba, "{}", path.display());
    buffer.truncate(info.buffer_size());
    (info.width, info.height, buffer)
}

/// Pulls `id` and compares it with `tests/golden/{name}.png`.
fn check(e: &mut Engine, id: OpId, name: &str) {
    let output = e.pull(id).unwrap_or_else(|error| panic!("{name}: {error}"));
    let texture = output.texture().expect("a texture");
    let (width, height) = (texture.width(), texture.height());
    let pixels = gpu().read_rgba8(texture.texture()).unwrap();
    let path = golden_dir().join(format!("{name}.png"));
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::create_dir_all(golden_dir()).unwrap();
        write_png(&path, width, height, &pixels);
        return;
    }
    assert!(
        path.exists(),
        "{} is missing: run with UPDATE_GOLDEN=1 to create it",
        path.display()
    );
    let (expected_width, expected_height, expected) = read_png(&path);
    assert_eq!((width, height), (expected_width, expected_height), "{name}'s size");
    let outliers = pixels
        .chunks(4)
        .zip(expected.chunks(4))
        .filter(|(a, b)| a.iter().zip(*b).any(|(a, b)| a.abs_diff(*b) > TOLERANCE))
        .count();
    let worst = pixels
        .iter()
        .zip(&expected)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    if outliers as f64 > OUTLIERS * f64::from(width * height) {
        let actual = std::env::temp_dir().join(format!("futhark-golden-{name}.png"));
        write_png(&actual, width, height, &pixels);
        panic!(
            "{name}: {outliers} pixels differ by more than {TOLERANCE} (at most {worst}); the actual image is at {}",
            actual.display()
        );
    }
}

#[test]
fn constant() {
    let mut e = engine();
    let id = add(
        &mut e,
        "c",
        "texture/constant",
        &sized(&[("r", f(0.9)), ("g", f(0.5)), ("b", f(0.25)), ("a", f(0.75))]),
    );
    check(&mut e, id, "constant");
}

#[test]
fn ramp() {
    let mut e = engine();
    let horizontal = add(&mut e, "h", "texture/ramp", &sized(&[]));
    check(&mut e, horizontal, "ramp_horizontal");
    let colors = [
        ("start_r", f(1.0)),
        ("start_g", f(0.2)),
        ("start_b", f(0.0)),
        ("end_r", f(0.0)),
        ("end_g", f(0.3)),
        ("end_b", f(1.0)),
        ("end_a", f(0.5)),
    ];
    let mut params = sized(&colors);
    params.push(("kind", menu("vertical")));
    let vertical = add(&mut e, "v", "texture/ramp", &params);
    check(&mut e, vertical, "ramp_vertical");
    let mut params = colors.to_vec();
    params.extend([
        ("kind", menu("radial")),
        ("width", Value::Int(96)),
        ("height", Value::Int(64)),
    ]);
    let radial = add(&mut e, "r", "texture/ramp", &params);
    check(&mut e, radial, "ramp_radial");
}

#[test]
fn noise() {
    let mut e = engine();
    e.set_frame(30);
    let mono = add(&mut e, "mono", "texture/noise", &sized(&[]));
    check(&mut e, mono, "noise_monochrome");
    let color = add(
        &mut e,
        "color",
        "texture/noise",
        &sized(&[
            ("seed", Value::Int(7)),
            ("octaves", Value::Int(4)),
            ("period", f(0.5)),
            ("monochrome", Value::Bool(false)),
        ]),
    );
    check(&mut e, color, "noise_color");
}

/// Red rising to the right plus green rising upward, so any flip or turn shows.
fn corners(e: &mut Engine) -> OpId {
    let red = add(
        e,
        "red",
        "texture/ramp",
        &sized(&[("end_g", f(0.0)), ("end_b", f(0.0))]),
    );
    let green = add(
        e,
        "green",
        "texture/ramp",
        &sized(&[("kind", menu("vertical")), ("end_r", f(0.0)), ("end_b", f(0.0))]),
    );
    let sum = add(e, "sum", "texture/composite", &[("operation", menu("add"))]);
    wire(e, &[red, green], sum);
    sum
}

#[test]
fn transform() {
    let mut e = engine();
    let source = corners(&mut e);
    for extend in ["hold", "zero", "repeat", "mirror"] {
        let id = add(
            &mut e,
            extend,
            "texture/transform",
            &[
                ("translate_x", f(0.1)),
                ("translate_y", f(0.05)),
                ("scale_x", f(0.6)),
                ("scale_y", f(0.4)),
                ("rotate", f(30.0)),
                ("extend", menu(extend)),
            ],
        );
        wire(&mut e, &[source], id);
        check(&mut e, id, &format!("transform_{extend}"));
    }
}

#[test]
fn level() {
    let mut e = engine();
    let source = corners(&mut e);
    let id = add(
        &mut e,
        "level",
        "texture/level",
        &[
            ("brightness", f(1.2)),
            ("contrast", f(1.5)),
            ("gamma", f(2.0)),
            ("opacity", f(0.8)),
            ("invert", f(0.25)),
        ],
    );
    wire(&mut e, &[source], id);
    check(&mut e, id, "level");
}

#[test]
fn composite() {
    let mut e = engine();
    // A red ramp fading in from the left, a half-transparent blue ramp rising upward and a 32×32 gray square,
    // stretched to the first input's size.
    let red = add(
        &mut e,
        "red",
        "texture/ramp",
        &sized(&[("start_a", f(0.0)), ("end_g", f(0.0)), ("end_b", f(0.0))]),
    );
    let blue = add(
        &mut e,
        "blue",
        "texture/ramp",
        &sized(&[
            ("kind", menu("vertical")),
            ("end_r", f(0.0)),
            ("end_g", f(0.0)),
            ("end_a", f(0.5)),
        ]),
    );
    let gray = add(
        &mut e,
        "gray",
        "texture/constant",
        &[
            ("width", Value::Int(32)),
            ("height", Value::Int(32)),
            ("r", f(0.5)),
            ("g", f(0.5)),
            ("b", f(0.5)),
            ("a", f(0.5)),
        ],
    );
    for operation in ["over", "add", "multiply", "screen", "difference", "maximum", "minimum"] {
        let id = add(
            &mut e,
            operation,
            "texture/composite",
            &[("operation", menu(operation))],
        );
        wire(&mut e, &[red, blue, gray], id);
        check(&mut e, id, &format!("composite_{operation}"));
    }
}

#[test]
fn shader() {
    let mut e = engine();
    e.set_frame(30);
    let plain = add(&mut e, "plain", "texture/shader", &sized(&[]));
    check(&mut e, plain, "shader_default");

    let source = corners(&mut e);
    let code = "@fragment
fn main(in: Varyings) -> @location(0) vec4f {
    let pixel = vec2i(in.position.xy);
    let checker = f32((pixel.x / 8 + pixel.y / 8) % 2);
    let color = textureSample(input0, sampler0, in.uv);
    return vec4f(mix(color.rgb, 1.0 - color.rgb, checker), 1.0) * f32(INPUTS) * futhark.resolution.x / 64.0;
}
";
    let id = add(
        &mut e,
        "inverted",
        "texture/shader",
        &[("code", Value::String(code.into()))],
    );
    wire(&mut e, &[source], id);
    check(&mut e, id, "shader_input");
}

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

#[test]
fn file_in() {
    let mut e = engine();
    // A 16×8 PNG loads at its own size, exactly as stored.
    let (width, height, pixels) = read_png(Path::new(&fixture("gradient.png")));
    let png = add(
        &mut e,
        "png",
        "texture/file_in",
        &[("path", Value::String(fixture("gradient.png")))],
    );
    let output = e.pull(png).unwrap();
    let texture = output.texture().unwrap();
    assert_eq!((texture.width(), texture.height()), (width, height));
    assert_eq!(gpu().read_rgba8(texture.texture()).unwrap(), pixels);

    let stretched = add(
        &mut e,
        "stretched",
        "texture/file_in",
        &sized(&[("path", Value::String(fixture("gradient.png")))]),
    );
    check(&mut e, stretched, "file_in_stretched");

    let jpeg = add(
        &mut e,
        "jpeg",
        "texture/file_in",
        &[("path", Value::String(fixture("checker.jpg")))],
    );
    check(&mut e, jpeg, "file_in_jpeg");
}
