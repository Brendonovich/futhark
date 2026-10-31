use std::{path::PathBuf, process::ExitCode};

use clap::Parser;

/// The Futhark server: owns the open project, the engine and the GPU. Clients connect over a Unix socket.
#[derive(Parser)]
#[command(name = "futharkd", version)]
struct Args {
    /// The socket to listen on. Defaults to $FUTHARK_SOCKET, then the user's state directory.
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Open this project at start.
    #[arg(long)]
    project: Option<PathBuf>,
    /// Shut down when standard input closes, for a parent process that owns the server (tests).
    #[arg(long)]
    exit_on_stdin_close: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let options = futhark_server::Options {
        socket: args.socket.unwrap_or_else(futhark_protocol::socket_path),
        project: args.project,
        exit_on_stdin_close: args.exit_on_stdin_close,
    };
    match futhark_server::run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("futharkd: {error}");
            ExitCode::FAILURE
        }
    }
}
