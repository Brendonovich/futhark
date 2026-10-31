use std::{path::PathBuf, process::ExitCode, sync::Arc};

use clap::{Parser, Subcommand};
use futhark_gpu::Gpu;
use futhark_graph::{Clock, Engine, Registry};
use futhark_project::{Project, create, load};

#[derive(Parser)]
#[command(name = "futhark", version, about = "A realtime node-based environment")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create an empty project.
    New {
        dir: PathBuf,
        /// The project's name. Defaults to the directory's name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Load a project and report anything that doesn't load cleanly.
    Check { project: PathBuf },
    /// Cook an operator for some frames and print its output.
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
    match run(Cli::parse().command) {
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

fn run(command: Command) -> Result<(), String> {
    match command {
        Command::New { dir, name } => {
            if dir.join("project.kdl").exists() {
                return Err(format!("{} already has a project", dir.display()));
            }
            let name = name
                .or_else(|| Some(dir.canonicalize().ok()?.file_name()?.to_string_lossy().into_owned()))
                .or_else(|| Some(dir.file_name()?.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "untitled".into());
            create(&dir, &Project::new(name, registry())).map_err(|e| e.to_string())?;
            println!("created {}", dir.display());
            Ok(())
        }
        Command::Check { project } => {
            let (_, diagnostics) = load(&project, registry()).map_err(|e| e.to_string())?;
            for diagnostic in &diagnostics {
                println!("{diagnostic}");
            }
            match diagnostics.len() {
                0 => Ok(()),
                1 => Err("1 problem".into()),
                n => Err(format!("{n} problems")),
            }
        }
        Command::Cook { project, op, frames } => {
            let (project, diagnostics) = load(&project, registry()).map_err(|e| e.to_string())?;
            for diagnostic in &diagnostics {
                eprintln!("warning: {diagnostic}");
            }
            let id = project
                .network
                .find(&op)
                .ok_or_else(|| format!("no operator at {op}"))?;
            let mut engine = Engine::new(
                project.network,
                Clock {
                    rate: project.rate,
                    frame: 0,
                },
            );
            for frame in 0..i64::from(frames) {
                engine.set_frame(frame);
                let output = engine.pull(id).map_err(|e| format!("{op}: {e}"))?;
                let channels = output.channels().expect("only Channel operators exist so far");
                let values: Vec<String> = channels
                    .channels()
                    .iter()
                    .map(|c| format!("{}={:.4}", c.name, c.samples.last().copied().unwrap_or(0.0)))
                    .collect();
                println!("{frame:>5}  {}", values.join("  "));
            }
            Ok(())
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
            Ok(())
        }
    }
}
