//! Optional Sun55i Cedar H.264 encoder bridge.

use crate::error::{AppError, Result};
use bytes::Bytes;

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
mod imp {
    use super::*;
    use libc::{c_int, c_void, dlclose, dlopen, dlsym, RTLD_LOCAL, RTLD_NOW};
    use std::mem;
    use std::ptr;

    const LIBRARY: &[u8] = b"/usr/lib/aarch64-linux-gnu/avaota-cedar/libavaota-cedar-bridge.so\0";

    type Open = unsafe extern "C" fn(c_int, c_int, c_int, c_int, c_int) -> *mut c_void;
    type Close = unsafe extern "C" fn(*mut c_void);
    type Encode = unsafe extern "C" fn(
        *mut c_void,
        *const u8,
        usize,
        i64,
        *mut u8,
        usize,
        *mut usize,
        *mut c_int,
    ) -> c_int;
    type SetBitrate = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
    type ForceKeyframe = unsafe extern "C" fn(*mut c_void) -> c_int;
    type ApiVersion = unsafe extern "C" fn() -> c_int;

    unsafe fn symbol<T: Copy>(library: *mut c_void, name: &[u8]) -> Option<T> {
        let address = dlsym(library, name.as_ptr().cast());
        if address.is_null() {
            None
        } else {
            Some(mem::transmute_copy(&address))
        }
    }

    pub fn available() -> bool {
        if !std::path::Path::new("/dev/cedar_dev").exists() {
            return false;
        }
        unsafe {
            let library = dlopen(LIBRARY.as_ptr().cast(), RTLD_NOW | RTLD_LOCAL);
            if library.is_null() {
                return false;
            }
            let valid = symbol::<ApiVersion>(library, b"avaota_cedar_api_version\0")
                .is_some_and(|version| version() == 1);
            dlclose(library);
            valid
        }
    }

    pub struct CedarH264 {
        library: *mut c_void,
        context: *mut c_void,
        close: Close,
        encode: Encode,
        set_bitrate: SetBitrate,
        force_keyframe: ForceKeyframe,
        output: Vec<u8>,
    }

    // The encoder is owned by one pipeline thread and is never called concurrently.
    unsafe impl Send for CedarH264 {}

    impl CedarH264 {
        pub fn new(width: u32, height: u32, fps: u32, bitrate_kbps: u32, gop: u32) -> Result<Self> {
            unsafe {
                let library = dlopen(LIBRARY.as_ptr().cast(), RTLD_NOW | RTLD_LOCAL);
                if library.is_null() {
                    return Err(AppError::VideoError("Cedar bridge is unavailable".into()));
                }
                let loaded = (|| {
                    let open = symbol::<Open>(library, b"avaota_cedar_h264_open\0")?;
                    let close = symbol::<Close>(library, b"avaota_cedar_h264_close\0")?;
                    let encode = symbol::<Encode>(library, b"avaota_cedar_h264_encode\0")?;
                    let set_bitrate =
                        symbol::<SetBitrate>(library, b"avaota_cedar_h264_set_bitrate\0")?;
                    let force_keyframe =
                        symbol::<ForceKeyframe>(library, b"avaota_cedar_h264_force_keyframe\0")?;
                    Some((open, close, encode, set_bitrate, force_keyframe))
                })();
                let Some((open, close, encode, set_bitrate, force_keyframe)) = loaded else {
                    dlclose(library);
                    return Err(AppError::VideoError(
                        "Cedar bridge API is incomplete".into(),
                    ));
                };
                let context = open(
                    width as c_int,
                    height as c_int,
                    fps as c_int,
                    bitrate_kbps.saturating_mul(1000) as c_int,
                    gop as c_int,
                );
                if context.is_null() {
                    dlclose(library);
                    return Err(AppError::VideoError(
                        "Cedar H.264 initialization failed".into(),
                    ));
                }
                let capacity = width as usize * height as usize * 3 / 2 + 1024 * 1024;
                Ok(Self {
                    library,
                    context,
                    close,
                    encode,
                    set_bitrate,
                    force_keyframe,
                    output: vec![0; capacity],
                })
            }
        }

        pub fn encode(&mut self, data: &[u8], pts_ms: i64) -> Result<(Bytes, bool)> {
            let mut written = 0;
            let mut key = 0;
            let result = unsafe {
                (self.encode)(
                    self.context,
                    data.as_ptr(),
                    data.len(),
                    pts_ms,
                    self.output.as_mut_ptr(),
                    self.output.len(),
                    &mut written,
                    &mut key,
                )
            };
            if result != 0 || written == 0 || written > self.output.len() {
                return Err(AppError::VideoError(format!(
                    "Cedar H.264 encoding failed: {result}"
                )));
            }
            Ok((Bytes::copy_from_slice(&self.output[..written]), key != 0))
        }

        pub fn set_bitrate(&mut self, bitrate_kbps: u32) -> Result<()> {
            let result = unsafe {
                (self.set_bitrate)(self.context, bitrate_kbps.saturating_mul(1000) as c_int)
            };
            if result != 0 {
                return Err(AppError::VideoError(format!(
                    "Cedar bitrate update failed: {result}"
                )));
            }
            Ok(())
        }

        pub fn request_keyframe(&mut self) {
            unsafe { (self.force_keyframe)(self.context) };
        }
    }

    impl Drop for CedarH264 {
        fn drop(&mut self) {
            unsafe {
                (self.close)(self.context);
                dlclose(self.library);
            }
            self.context = ptr::null_mut();
        }
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "aarch64")))]
mod imp {
    use super::*;

    pub fn available() -> bool {
        false
    }

    pub struct CedarH264;
    impl CedarH264 {
        pub fn new(_: u32, _: u32, _: u32, _: u32, _: u32) -> Result<Self> {
            Err(AppError::VideoError("Cedar requires Linux arm64".into()))
        }
        pub fn encode(&mut self, _: &[u8], _: i64) -> Result<(Bytes, bool)> {
            Err(AppError::VideoError("Cedar requires Linux arm64".into()))
        }
        pub fn set_bitrate(&mut self, _: u32) -> Result<()> {
            Err(AppError::VideoError("Cedar requires Linux arm64".into()))
        }
        pub fn request_keyframe(&mut self) {}
    }
}

pub use imp::{available, CedarH264};
