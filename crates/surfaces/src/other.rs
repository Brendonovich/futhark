//! Systems without shared surfaces yet: the same API, every constructor returning [`Error::Unsupported`].

use std::ffi::c_void;
use std::time::Duration;

use crate::{Error, Token};

/// A picture in GPU memory several processes can use at once (not available on this system).
#[derive(Clone, Debug)]
pub struct Surface(());

impl Surface {
    pub fn bgra(_width: u32, _height: u32) -> Result<Self, Error> {
        Err(Error::Unsupported)
    }

    pub fn width(&self) -> u32 {
        0
    }

    pub fn height(&self) -> u32 {
        0
    }

    pub fn as_ptr(&self) -> *mut c_void {
        std::ptr::null_mut()
    }

    pub fn texture(&self, _device: &wgpu::Device, _usage: wgpu::TextureUsages) -> Result<wgpu::Texture, Error> {
        Err(Error::Unsupported)
    }

    pub fn read_bgra(&self) -> Vec<u8> {
        Vec::new()
    }
}

/// The daemon's side of handing surfaces over (not available on this system).
pub struct Exporter(());

impl Exporter {
    pub fn shared() -> Result<&'static Exporter, Error> {
        Err(Error::Unsupported)
    }

    pub fn service_name(&self) -> &str {
        ""
    }

    pub fn offer(&self, _surfaces: Vec<Surface>) -> Token {
        Token([0; 16])
    }

    pub fn withdraw(&self, _token: &Token) {}
}

pub fn fetch(_service: &str, _token: &str, _timeout: Duration) -> Result<Vec<Surface>, Error> {
    Err(Error::Unsupported)
}
