//! GPU surfaces the daemon shares with its clients without copying: an operator viewer's picture is drawn by the
//! daemon's GPU into a small ring of surfaces, and the client's GPU shows them in place.
//!
//! On macOS a surface is an `IOSurface`. A Unix socket can't carry one, so the surfaces of a ring are handed over once,
//! as Mach ports (`IOSurfaceCreateMachPort`), through a Mach service the daemon registers with the bootstrap server
//! under a name of its own ([`Exporter`]). The client is told that name and a one-time [`Token`] over the control
//! socket and fetches the ring with [`fetch`]. After that only slot numbers travel: the daemon names the slot holding a
//! new frame, and the client releases it when its GPU is done with it.
//!
//! On other systems everything here returns [`Error::Unsupported`].
//!
//! The design and much of the code are adapted from Isocast's `isocast-surfaces` crate
//! (<https://github.com/R44VC0RP/isocast>, MIT/Apache-2.0); its `docs/shared-frames.md` explains the transport.

// IOSurface, Mach and the Metal interop of wgpu-hal are C and Objective-C APIs with no safe wrappers, so this crate
// is the one place the engine allows unsafe code. Every unsafe block says why it is sound.
#![allow(unsafe_code)]

use std::fmt;
use std::str::FromStr;

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
pub use mac::{Exporter, Surface, fetch};

#[cfg(not(target_os = "macos"))]
mod other;
#[cfg(not(target_os = "macos"))]
pub use other::{Exporter, Surface, fetch};

/// How surfaces travel between processes on this system, as told to clients.
#[cfg(target_os = "macos")]
pub const TRANSPORT: &str = "iosurface-mach";
#[cfg(not(target_os = "macos"))]
pub const TRANSPORT: &str = "unsupported";

/// The most surfaces one offer may hold.
pub const MAX_SURFACES: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("shared GPU surfaces are only available on macOS so far")]
    Unsupported,
    #[error("a surface can't be {0}×{1}")]
    Size(u32, u32),
    #[error("could not create a {0}×{1} IOSurface")]
    Create(u32, u32),
    #[error("the GPU is not a Metal device")]
    NotMetal,
    #[error("Metal could not make a texture of the surface")]
    Texture,
    #[error("could not register the Mach service {service}: {reason}")]
    Register { service: String, reason: String },
    #[error(
        "could not find the surface service {service} ({reason}); the daemon may have stopped, or run in another \
         login session"
    )]
    Lookup { service: String, reason: String },
    #[error("could not {doing} (Mach error {code:#x})")]
    Mach { doing: &'static str, code: i32 },
    #[error("the surface service did not answer within {0:?}")]
    Timeout(std::time::Duration),
    #[error("the surface service refused the token: it was used already or withdrawn")]
    Refused,
    #[error("an unexpected reply from the surface service")]
    BadReply,
    #[error("the surface service sent a surface this process could not open")]
    Open,
    #[error("token: {0:?} is not 32 hex digits")]
    InvalidToken(String),
    #[error("{0}")]
    Service(String),
}

/// A secret that lets one client fetch one offer's surfaces, once. Shown and parsed as 32 hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Token(pub [u8; 16]);

impl Token {
    /// A random token from the kernel's generator.
    pub fn random() -> Self {
        let mut bytes = [0u8; 16];
        fill_random(&mut bytes);
        Self(bytes)
    }
}

impl FromStr for Token {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self, Error> {
        let trimmed = text.trim();
        if trimmed.len() != 32 || !trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::InvalidToken(text.to_owned()));
        }
        let mut bytes = [0u8; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&trimmed[index * 2..index * 2 + 2], 16)
                .map_err(|_| Error::InvalidToken(text.to_owned()))?;
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Tokens are secrets; keep them out of logs.
        f.write_str("Token(…)")
    }
}

#[cfg(unix)]
fn fill_random(bytes: &mut [u8]) {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(bytes))
        .expect("could not read /dev/urandom");
}

#[cfg(not(unix))]
fn fill_random(bytes: &mut [u8]) {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    for chunk in bytes.chunks_mut(8) {
        let value = RandomState::new().build_hasher().finish().to_le_bytes();
        chunk.copy_from_slice(&value[..chunk.len()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_read_back_as_shown() {
        let token = Token::random();
        assert_eq!(token.to_string().parse::<Token>().unwrap(), token);
        assert_ne!(Token::random(), token);
        assert!(matches!("xyz".parse::<Token>(), Err(Error::InvalidToken(_))));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unsupported_elsewhere() {
        assert_eq!(TRANSPORT, "unsupported");
        assert!(matches!(Surface::bgra(4, 4), Err(Error::Unsupported)));
        assert!(matches!(Exporter::shared(), Err(Error::Unsupported)));
        assert!(matches!(
            fetch("x", &Token::random().to_string(), std::time::Duration::from_millis(1)),
            Err(Error::Unsupported)
        ));
    }
}
