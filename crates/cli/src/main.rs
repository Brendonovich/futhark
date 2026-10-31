use std::{path::PathBuf, process::ExitCode, sync::Arc};

use clap::{Parser, Subcommand};
use futhark_client::Client;
use futhark_gpu::Gpu;
use futhark_graph::{Clock, Engine, Registry};
use futhark_project::{Project, create, load};
use futhark_protocol::{Edit, Event, Method, Sample, Snapshot, Value};
use serde::Serialize;

#[derive(Parser)]
#[command(name = "futhark", version, about = "A realtime node-based environment")]
struct Cli {
    /// The server's socket. Defaults to $FUTHARK_SOCKET, then the user's state directory.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    /// Print results as JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a project and open it in the server.
    Create {
        dir: PathBuf,
        /// Defaults to the directory's name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Open a project in the server.
    Open { dir: PathBuf },
    /// Save the open project.
    Save,
    /// Close the open project without saving.
    Close,
    /// Show the server's status.
    Status,
    /// List the open project's operators.
    Get,
    /// List every operator type and its parameters.
    Types,
    /// Add an operator, such as `futhark add /fx/noise1 channel/noise`.
    Add {
        path: String,
        #[arg(value_name = "TYPE")]
        ty: String,
    },
    /// Remove an operator.
    Remove { path: String },
    /// Rename an operator.
    Rename { path: String, name: String },
    /// Set a parameter. Values are parsed as integers, floats, true/false, or else strings.
    Set { path: String, param: String, value: String },
    /// Wire one operator's output into another's input.
    Connect {
        from: String,
        to: String,
        #[arg(long, default_value_t = 0)]
        input: usize,
    },
    /// Remove the wire into an input.
    Disconnect {
        to: String,
        #[arg(long, default_value_t = 0)]
        input: usize,
    },
    /// Move an operator in the network editor.
    Move { path: String, x: f32, y: f32 },
    /// Print an operator's current output.
    Sample { path: String },
    /// Print the server's events as they happen.
    Watch,
    /// Watch a Texture operator through shared GPU surfaces and print each frame.
    View {
        path: String,
        /// Stop after this many frames.
        #[arg(long, default_value_t = 10)]
        frames: u32,
        /// The largest picture to ask for, in pixels on each side.
        #[arg(long, default_value_t = 640)]
        max: u32,
        /// Read every frame's pixels back and fail on one that is fully transparent, which would mean a slot was
        /// announced before it was drawn.
        #[arg(long)]
        check: bool,
    },
    /// Call any method directly: `futhark call edit '{"edits": [...]}'`. Prints the JSON result.
    Call {
        method: String,
        /// The method's params, as JSON.
        params: Option<String>,
    },
    /// Stop the server.
    Shutdown,

    /// Create an empty project on disk, without the server.
    New {
        dir: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// Load a project from disk and report anything that doesn't load cleanly. Doesn't use the server.
    Check { project: PathBuf },
    /// Cook an operator offline for some frames and print its output. Doesn't use the server.
    Cook {
        project: PathBuf,
        /// The operator's path, such as /fx/noise1.
        op: String,
        #[arg(long, default_value_t = 1)]
        frames: u32,
    },
    /// Show the GPU adapter the engine would use.
    Gpu,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn registry() -> Arc<Registry> {
    Arc::new(futhark_ops::registry())
}

fn connect(cli_socket: &Option<PathBuf>, launch: bool) -> Result<Client, String> {
    let socket = cli_socket.clone().unwrap_or_else(futhark_protocol::socket_path);
    let client = if launch {
        Client::connect_or_launch_at(&socket)
    } else {
        Client::connect(&socket)
    };
    let client = client.map_err(|e| e.to_string())?;
    if let Some(stale) = client.stale() {
        eprintln!("warning: {stale}");
    }
    Ok(client)
}

fn edit(cli: &Cli, edit: Edit) -> Result<(), String> {
    let edited = connect(&cli.socket, true)?
        .edit(None, vec![edit])
        .map_err(|e| e.to_string())?;
    print(cli.json, &edited, || format!("revision {}", edited.revision));
    Ok(())
}

/// Prints `value` as JSON with `--json`, otherwise the human form.
fn print<T: Serialize>(json: bool, value: &T, human: impl FnOnce() -> String) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(value).expect("protocol types serialize")
        );
    } else {
        let text = human();
        if !text.is_empty() {
            println!("{text}");
        }
    }
}

fn parse_value(text: &str) -> Value {
    if let Ok(v) = text.parse::<i64>() {
        Value::Int(v)
    } else if let Ok(v) = text.parse::<f64>() {
        Value::Float(v)
    } else if let Ok(v) = text.parse::<bool>() {
        Value::Bool(v)
    } else {
        Value::String(text.into())
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let err = |e: futhark_client::ClientError| e.to_string();
    match &cli.command {
        Command::Create { dir, name } => {
            let opened = connect(&cli.socket, true)?.create(dir, name.clone()).map_err(err)?;
            print(cli.json, &opened, || {
                format!("created and opened {}", opened.project.path)
            });
        }
        Command::Open { dir } => {
            let opened = connect(&cli.socket, true)?.open(dir).map_err(err)?;
            print(cli.json, &opened, || {
                let mut lines = vec![format!("opened {} ({})", opened.project.name, opened.project.path)];
                lines.extend(opened.diagnostics.iter().map(|d| format!("warning: {d}")));
                lines.join("\n")
            });
        }
        Command::Save => connect(&cli.socket, false)?.save().map_err(err)?,
        Command::Close => connect(&cli.socket, false)?.close().map_err(err)?,
        Command::Status => {
            let status = connect(&cli.socket, false)?.status().map_err(err)?;
            print(cli.json, &status, || match &status.project {
                Some(project) => format!("{} ({}) at frame {}", project.name, project.path, status.frame),
                None => "no project open".into(),
            });
        }
        Command::Get => {
            let snapshot = connect(&cli.socket, true)?.get().map_err(err)?;
            print(cli.json, &snapshot, || describe(&snapshot));
        }
        Command::Types => {
            let types = connect(&cli.socket, true)?.types().map_err(err)?;
            print(cli.json, &types, || {
                let mut lines = Vec::new();
                for ty in &types {
                    lines.push(format!("{}  {}: {}", ty.name, ty.label, ty.doc));
                    for param in &ty.params {
                        lines.push(format!("    {} = {}", param.name, param.default));
                    }
                }
                lines.join("\n")
            });
        }
        Command::Add { path, ty } => edit(
            &cli,
            Edit::Add {
                path: path.clone(),
                ty: ty.clone(),
            },
        )?,
        Command::Remove { path } => edit(&cli, Edit::Remove { path: path.clone() })?,
        Command::Rename { path, name } => edit(
            &cli,
            Edit::Rename {
                path: path.clone(),
                name: name.clone(),
            },
        )?,
        Command::Set { path, param, value } => edit(
            &cli,
            Edit::Set {
                path: path.clone(),
                param: param.clone(),
                value: parse_value(value),
            },
        )?,
        Command::Connect { from, to, input } => edit(
            &cli,
            Edit::Connect {
                from: from.clone(),
                to: to.clone(),
                input: *input,
            },
        )?,
        Command::Disconnect { to, input } => edit(
            &cli,
            Edit::Disconnect {
                to: to.clone(),
                input: *input,
            },
        )?,
        Command::Move { path, x, y } => edit(
            &cli,
            Edit::Move {
                path: path.clone(),
                x: *x,
                y: *y,
            },
        )?,
        Command::Sample { path } => {
            let sample = connect(&cli.socket, true)?.sample(path).map_err(err)?;
            print(cli.json, &sample, || match &sample {
                Sample::Channel { frame, channels, .. } => {
                    let values: Vec<String> = channels
                        .iter()
                        .map(|c| format!("{}={:.4}", c.name, c.samples.last().copied().unwrap_or(0.0)))
                        .collect();
                    format!("frame {frame}  {}", values.join("  "))
                }
                Sample::Texture {
                    frame,
                    width,
                    height,
                    format,
                } => {
                    format!("frame {frame}  {width}×{height} {format}")
                }
            });
        }
        Command::View {
            path,
            frames,
            max,
            check,
        } => view(&cli, path, *frames, *max, *check)?,
        Command::Watch => {
            let (_, events) = connect(&cli.socket, true)?.watch().map_err(err)?;
            for event in events {
                let event = event.map_err(err)?;
                print(cli.json, &event, || match &event {
                    Event::NetworkChanged { revision } => format!("network changed (revision {revision})"),
                    Event::ProjectOpened { project } => format!("opened {} ({})", project.name, project.path),
                    Event::ProjectClosed => "closed".into(),
                    Event::ProjectSaved { project } => format!("saved {}", project.path),
                });
            }
        }
        Command::Call { method, params } => {
            let params: serde_json::Value = match params {
                Some(params) => serde_json::from_str(params).map_err(|e| format!("params: {e}"))?,
                None => serde_json::Value::Null,
            };
            let mut request = serde_json::json!({ "method": method });
            if !params.is_null() {
                request["params"] = params;
            }
            let method: Method = serde_json::from_value(request).map_err(|e| format!("{method}: {e}"))?;
            if matches!(method, Method::Watch) {
                return Err("use `futhark watch` to watch events".into());
            }
            let result = connect(&cli.socket, true)?.call_value(method).map_err(err)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&result).expect("JSON values serialize")
            );
        }
        Command::Shutdown => connect(&cli.socket, false)?.shutdown().map_err(err)?,

        Command::New { dir, name } => {
            let name = name
                .clone()
                .or_else(|| {
                    Some(
                        std::path::absolute(dir)
                            .ok()?
                            .file_name()?
                            .to_string_lossy()
                            .into_owned(),
                    )
                })
                .unwrap_or_else(|| "untitled".into());
            create(dir, &Project::new(name, registry())).map_err(|e| e.to_string())?;
            println!("created {}", dir.display());
        }
        Command::Check { project } => {
            let (_, diagnostics) = load(project, registry()).map_err(|e| e.to_string())?;
            for diagnostic in &diagnostics {
                println!("{diagnostic}");
            }
            match diagnostics.len() {
                0 => {}
                1 => return Err("1 problem".into()),
                n => return Err(format!("{n} problems")),
            }
        }
        Command::Cook { project, op, frames } => {
            let project_dir = project.clone();
            let (project, diagnostics) = load(project, registry()).map_err(|e| e.to_string())?;
            for diagnostic in &diagnostics {
                eprintln!("warning: {diagnostic}");
            }
            let id = project.network.find(op).ok_or_else(|| format!("no operator at {op}"))?;
            let clock = Clock {
                rate: project.rate,
                frame: 0,
            };
            let dir = std::path::absolute(&project_dir).map_err(|e| e.to_string())?;
            let mut engine = Engine::new(project.network, clock).with_project_dir(dir);
            match Gpu::headless() {
                Ok(gpu) => engine = engine.with_gpu(Arc::new(gpu)),
                Err(error) => eprintln!("warning: no GPU, so Texture operators won't cook: {error}"),
            }
            for frame in 0..i64::from(*frames) {
                engine.set_frame(frame);
                let output = engine.pull(id).map_err(|e| format!("{op}: {e}"))?;
                let Some(channels) = output.channels() else {
                    let texture = output.texture().expect("Channel or Texture");
                    println!(
                        "{frame:>5}  {}×{} {:?}",
                        texture.width(),
                        texture.height(),
                        texture.format()
                    );
                    continue;
                };
                let values: Vec<String> = channels
                    .channels()
                    .iter()
                    .map(|c| format!("{}={:.4}", c.name, c.samples.last().copied().unwrap_or(0.0)))
                    .collect();
                println!("{frame:>5}  {}", values.join("  "));
            }
        }
        Command::Gpu => {
            let gpu = Gpu::headless().map_err(|e| e.to_string())?;
            let info = gpu.adapter.get_info();
            println!("adapter: {}", info.name);
            println!("backend: {}", info.backend);
            println!("type: {:?}", info.device_type);
            if !info.driver.is_empty() {
                println!("driver: {} {}", info.driver, info.driver_info);
            }
        }
    }
    Ok(())
}

fn view(cli: &Cli, path: &str, frames: u32, max: u32, check: bool) -> Result<(), String> {
    let params = futhark_protocol::shared::Params {
        path: path.into(),
        max_width: max,
        max_height: max,
        fps: None,
        slots: None,
    };
    let mut stream = connect(&cli.socket, true)?
        .shared_frames(params)
        .map_err(|e| e.to_string())?;
    let mut seen = 0;
    while seen < frames {
        match stream.recv().map_err(|e| e.to_string())? {
            None => return Err("the stream ended".into()),
            Some(futhark_client::Update::Error(message)) => println!("error: {message}"),
            Some(futhark_client::Update::Frame(frame)) => {
                seen += 1;
                let (width, height) = (frame.surface.width(), frame.surface.height());
                let mut line = format!("frame {}  {width}×{height}  slot {}", frame.frame, frame.slot);
                if check {
                    let pixels = frame.surface.read_bgra();
                    if pixels.chunks(4).all(|p| p == [0, 0, 0, 0]) {
                        return Err(format!("frame {} is empty", frame.frame));
                    }
                    line.push_str("  ok");
                }
                println!("{line}");
            }
        }
    }
    Ok(())
}

fn describe(snapshot: &Snapshot) -> String {
    let mut lines = vec![format!("{} (revision {})", snapshot.project.name, snapshot.revision)];
    for op in &snapshot.ops {
        let mut line = format!("{}  {}", op.path, op.ty);
        let inputs: Vec<&str> = op.inputs.iter().map(|i| i.as_deref().unwrap_or("-")).collect();
        if !inputs.is_empty() {
            line.push_str(&format!("  <- {}", inputs.join(", ")));
        }
        if let Some(error) = &op.error {
            line.push_str(&format!("  error: {error}"));
        }
        lines.push(line);
    }
    lines.join("\n")
}
