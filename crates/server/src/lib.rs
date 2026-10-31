//! `futharkd`: the server. One thread owns the engine and the open project ([`core`]); each client connection gets a
//! thread that parses requests and hands them to it.

mod convert;
mod core;

use std::{
    fs,
    io::{self, BufRead, BufReader, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};

use futhark_protocol::{Method, Notification, OpenParams, Request, Response, RpcError};

use crate::core::Core;

pub struct Options {
    pub socket: PathBuf,
    pub project: Option<PathBuf>,
    pub exit_on_stdin_close: bool,
}

/// Runs the server until a client asks it to shut down.
pub fn run(options: Options) -> io::Result<()> {
    let listener = bind(&options.socket)?;
    let core = core::spawn();

    if let Some(project) = options.project {
        let path = std::path::absolute(project)?.to_string_lossy().into_owned();
        core.call(move |core: &mut Core| core.handle(Method::Open(OpenParams { path })))
            .map_err(|e| io::Error::other(e.message))?;
    }

    let (stop, stopped) = mpsc::channel::<()>();
    if options.exit_on_stdin_close {
        let stop = stop.clone();
        thread::spawn(move || {
            let _ = io::copy(&mut io::stdin(), &mut io::sink());
            let _ = stop.send(());
        });
    }

    let socket = options.socket.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let (core, stop) = (core.clone(), stop.clone());
            thread::spawn(move || {
                if let Err(error) = serve(stream, &core, &stop)
                    && error.kind() != io::ErrorKind::BrokenPipe
                {
                    eprintln!("futharkd: connection: {error}");
                }
            });
        }
    });

    let _ = stopped.recv();
    let _ = fs::remove_file(&socket);
    Ok(())
}

/// Listens on `path`, owner-only. A socket left by a server that's gone is replaced; a live one is an error.
fn bind(path: &Path) -> io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("futharkd is already running at {}", path.display()),
            ));
        }
        fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn serve(stream: UnixStream, core: &core::Handle, stop: &mpsc::Sender<()>) -> io::Result<()> {
    let mut writer = stream.try_clone()?;
    for line in BufReader::new(stream).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request = match parse(&line) {
            Ok(request) => request,
            Err(response) => {
                write_line(&mut writer, &response)?;
                continue;
            }
        };
        let id = request.id;
        match request.method {
            Method::Watch => {
                let (events, status) = core.call(|core: &mut Core| core.subscribe());
                write_line(&mut writer, &Response::ok(id, to_value(&status)))?;
                for event in events {
                    write_line(&mut writer, &Notification::event(event))?;
                }
                return Ok(());
            }
            Method::Shutdown => {
                write_line(&mut writer, &Response::ok(id, serde_json::Value::Null))?;
                let _ = stop.send(());
                return Ok(());
            }
            method => {
                let response = match core.call(move |core: &mut Core| core.handle(method)) {
                    Ok(result) => Response::ok(id, result),
                    Err(error) => Response::error(Some(id), error),
                };
                write_line(&mut writer, &response)?;
            }
        }
    }
    Ok(())
}

/// Parses a request line, or makes the error response for it.
fn parse(line: &str) -> Result<Request, Response> {
    let value: serde_json::Value = serde_json::from_str(line)
        .map_err(|e| Response::error(None, RpcError::new(RpcError::PARSE_ERROR, e.to_string())))?;
    let id = value.get("id").and_then(serde_json::Value::as_u64);
    let method = value
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    serde_json::from_value(value).map_err(|e| {
        let message = e.to_string();
        let code = if message.starts_with("unknown variant") {
            RpcError::METHOD_NOT_FOUND
        } else if id.is_none() {
            RpcError::INVALID_REQUEST
        } else {
            RpcError::INVALID_PARAMS
        };
        Response::error(id, RpcError::new(code, format!("{method}: {message}")))
    })
}

fn write_line(writer: &mut impl Write, message: &impl serde::Serialize) -> io::Result<()> {
    let mut line = serde_json::to_string(message).map_err(io::Error::other)?;
    line.push('\n');
    writer.write_all(line.as_bytes())
}

fn to_value(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("protocol types serialize")
}
