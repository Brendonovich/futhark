use std::process::ExitCode;

use clap::{Parser, Subcommand};
use futhark_gpu::Gpu;

#[derive(Parser)]
#[command(name = "futhark", version, about = "A realtime node-based environment")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show the GPU adapter the engine would use.
    Gpu,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Gpu => match Gpu::headless() {
            Ok(gpu) => {
                let info = gpu.adapter.get_info();
                println!("adapter: {}", info.name);
                println!("backend: {}", info.backend);
                println!("type: {:?}", info.device_type);
                if !info.driver.is_empty() {
                    println!("driver: {} {}", info.driver, info.driver_info);
                }
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
    }
}
