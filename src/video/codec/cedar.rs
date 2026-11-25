//! Optional Sun55i Cedar H.264/H.265 encoder bridge.

use crate::error::{AppError, Result};
use bytes::Bytes;
use std::os::fd::OwnedFd;

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
mod imp {
    use super::*;
    use libc::{c_int, c_void, dlclose, dlopen, dlsym, RTLD_LOCAL, RTLD_NOW};
    use std::mem;
    use std::os::fd::AsRawFd;
    use std::ptr;

    const LIBRARY: &[u8] = b"/usr/lib/aarch64-linux-gnu/avaota-cedar/libavaota-cedar-bridge.so\0";

    type Open = unsafe extern "C" fn(c_int, c_int, c_int, c_int, c_int) -> *mut c_void;
    type OpenScaled = unsafe extern "C" fn(c_int, c_int, c_int, c_int, c_int, c_int, c_int) -> *mut c_void;
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
    type Import = unsafe extern "C" fn(*mut c_void, c_int, *mut u32) -> c_int;
    type Unimport = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
    type EncodeImported = unsafe extern "C" fn(
        *mut c_void, u32, i64, *mut u8, usize, *mut usize, *mut c_int,
    ) -> c_int;

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
                .is_some_and(|version| version() >= 1);
            dlclose(library);
            valid
        }
    }

    pub fn h265_available() -> bool {
        unsafe {
            let library = dlopen(LIBRARY.as_ptr().cast(), RTLD_NOW | RTLD_LOCAL);
            if library.is_null() { return false; }
            let valid = if symbol::<ApiVersion>(library, b"avaota_cedar_api_version\0")
                .is_some_and(|version| version() >= 4) {
                match (symbol::<OpenScaled>(library, b"avaota_cedar_h265_open_scaled\0"),
                       symbol::<Close>(library, b"avaota_cedar_h264_close\0")) {
                    (Some(open), Some(close)) => {
                        let context = open(640, 480, 640, 480, 30, 1_000_000, 30);
                        if context.is_null() { false } else { close(context); true }
                    }
                    _ => false,
                }
            } else { false };
            dlclose(library);
            valid && std::path::Path::new("/dev/cedar_dev").exists()
        }
    }

    pub fn jpeg_available() -> bool {
        static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *SUPPORTED.get_or_init(|| available() && CedarH264::new_jpeg(640, 480, 80).is_ok())
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
        pub fn new_jpeg(width: u32, height: u32, quality: u32) -> Result<Self> {
            unsafe {
                let library = dlopen(LIBRARY.as_ptr().cast(), RTLD_NOW | RTLD_LOCAL);
                if library.is_null() {
                    return Err(AppError::VideoError("Cedar bridge is unavailable".into()));
                }
                let loaded = (|| {
                    Some((
                        symbol::<OpenScaled>(library, b"avaota_cedar_jpeg_open_scaled\0")?,
                        symbol::<Close>(library, b"avaota_cedar_h264_close\0")?,
                        symbol::<Encode>(library, b"avaota_cedar_h264_encode\0")?,
                        symbol::<SetBitrate>(library, b"avaota_cedar_h264_set_bitrate\0")?,
                        symbol::<ForceKeyframe>(library, b"avaota_cedar_h264_force_keyframe\0")?,
                    ))
                })();
                let Some((open, close, encode, set_bitrate, force_keyframe)) = loaded else {
                    dlclose(library);
                    return Err(AppError::VideoError("Cedar JPEG API is unavailable".into()));
                };
                let context = open(width as c_int, height as c_int, width as c_int,
                                   height as c_int, 30, 4_000_000, quality as c_int);
                if context.is_null() {
                    dlclose(library);
                    return Err(AppError::VideoError("Cedar JPEG initialization failed".into()));
                }
                Ok(Self { library, context, close, encode, set_bitrate, force_keyframe,
                    output: vec![0; width as usize * height as usize * 2 + 1024 * 1024] })
            }
        }
        pub fn new(width: u32, height: u32, fps: u32, bitrate_kbps: u32, gop: u32) -> Result<Self> {
            Self::new_scaled(width, height, width, height, fps, bitrate_kbps, gop)
        }

        pub fn new_scaled(width: u32, height: u32, output_width: u32, output_height: u32, fps: u32, bitrate_kbps: u32, gop: u32) -> Result<Self> {
            Self::new_scaled_codec(false, width, height, output_width, output_height, fps, bitrate_kbps, gop)
        }

        pub fn new_scaled_codec(hevc: bool, width: u32, height: u32, output_width: u32, output_height: u32, fps: u32, bitrate_kbps: u32, gop: u32) -> Result<Self> {
            unsafe {
                let library = dlopen(LIBRARY.as_ptr().cast(), RTLD_NOW | RTLD_LOCAL);
                if library.is_null() {
                    return Err(AppError::VideoError("Cedar bridge is unavailable".into()));
                }
                let loaded = (|| {
                    let open = symbol::<Open>(library, b"avaota_cedar_h264_open\0")?;
                    let open_scaled = symbol::<OpenScaled>(library, if hevc { b"avaota_cedar_h265_open_scaled\0" } else { b"avaota_cedar_h264_open_scaled\0" });
                    let close = symbol::<Close>(library, b"avaota_cedar_h264_close\0")?;
                    let encode = symbol::<Encode>(library, b"avaota_cedar_h264_encode\0")?;
                    let set_bitrate =
                        symbol::<SetBitrate>(library, b"avaota_cedar_h264_set_bitrate\0")?;
                    let force_keyframe =
                        symbol::<ForceKeyframe>(library, b"avaota_cedar_h264_force_keyframe\0")?;
                    Some((open, open_scaled, close, encode, set_bitrate, force_keyframe))
                })();
                let Some((open, open_scaled, close, encode, set_bitrate, force_keyframe)) = loaded else {
                    dlclose(library);
                    return Err(AppError::VideoError(
                        "Cedar bridge API is incomplete".into(),
                    ));
                };
                let context = if !hevc && width == output_width && height == output_height {
                    open(width as c_int, height as c_int, fps as c_int,
                         bitrate_kbps.saturating_mul(1000) as c_int, gop as c_int)
                } else {
                    let open_scaled = open_scaled.ok_or_else(|| AppError::VideoError("Cedar bridge scaling is unavailable".into()))?;
                    open_scaled(width as c_int, height as c_int,
                                output_width as c_int, output_height as c_int,
                                fps as c_int, bitrate_kbps.saturating_mul(1000) as c_int,
                                gop as c_int)
                };
                if context.is_null() {
                    dlclose(library);
                    return Err(AppError::VideoError(
                        "Cedar encoder initialization failed".into(),
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

    pub struct CedarDma {
        encoder: CedarH264,
        imports: Vec<(OwnedFd, u32)>,
        unimport: Unimport,
        encode_imported: EncodeImported,
    }

    impl CedarDma {
        pub fn new(
            hevc: bool, width: u32, height: u32, output_width: u32, output_height: u32, stride: u32, fps: u32,
            bitrate_kbps: u32, gop: u32, buffers: Vec<(OwnedFd, usize)>,
        ) -> Result<Self> {
            if stride != width || buffers.len() < 2 {
                return Err(AppError::VideoError("Cedar DMA needs tight NV12 buffers".into()));
            }
            let minimum = width as usize * height.div_ceil(16) as usize * 16 * 3 / 2;
            if buffers.iter().any(|(_, size)| *size < minimum) {
                return Err(AppError::VideoError("Cedar DMA buffer is too small".into()));
            }
            let encoder = CedarH264::new_scaled_codec(hevc, width, height, output_width, output_height, fps, bitrate_kbps, gop)?;
            let (import, unimport, encode_imported) = unsafe {
                let library = encoder.library;
                let version = symbol::<ApiVersion>(library, b"avaota_cedar_api_version\0")
                    .ok_or_else(|| AppError::VideoError("Cedar bridge version unavailable".into()))?;
                if version() < 2 {
                    return Err(AppError::VideoError("Cedar bridge DMA API unavailable".into()));
                }
                (
                    symbol::<Import>(library, b"avaota_cedar_h264_import\0"),
                    symbol::<Unimport>(library, b"avaota_cedar_h264_unimport\0"),
                    symbol::<EncodeImported>(library, b"avaota_cedar_h264_encode_imported\0"),
                )
            };
            let (Some(import), Some(unimport), Some(encode_imported)) =
                (import, unimport, encode_imported) else {
                    return Err(AppError::VideoError("Cedar bridge DMA API incomplete".into()));
                };
            let mut dma = Self { encoder, imports: Vec::new(), unimport, encode_imported };
            for (fd, _) in buffers {
                let mut address = 0u32;
                let result = unsafe { import(dma.encoder.context, fd.as_raw_fd(), &mut address) };
                if result != 0 || address == 0 {
                    return Err(AppError::VideoError(format!(
                        "Cedar DMA import failed: {result}"
                    )));
                }
                dma.imports.push((fd, address));
            }
            Ok(dma)
        }

        pub fn encode(&mut self, index: usize, pts_ms: i64, force_keyframe: bool) -> Result<(Bytes, bool)> {
            let address = self.imports.get(index).ok_or_else(||
                AppError::VideoError("Invalid Cedar DMA buffer index".into()))?.1;
            if force_keyframe { self.encoder.request_keyframe(); }
            let mut written = 0;
            let mut key = 0;
            let result = unsafe {
                (self.encode_imported)(
                    self.encoder.context, address, pts_ms,
                    self.encoder.output.as_mut_ptr(), self.encoder.output.len(),
                    &mut written, &mut key,
                )
            };
            if result != 0 || written == 0 || written > self.encoder.output.len() {
                return Err(AppError::VideoError(format!(
                    "Cedar DMA encoding failed: {result}"
                )));
            }
            Ok((Bytes::copy_from_slice(&self.encoder.output[..written]), key != 0))
        }

        pub fn set_bitrate(&mut self, bitrate_kbps: u32) -> Result<()> {
            self.encoder.set_bitrate(bitrate_kbps)
        }
    }

    impl Drop for CedarDma {
        fn drop(&mut self) {
            for (fd, _) in &self.imports {
                let result = unsafe { (self.unimport)(self.encoder.context, fd.as_raw_fd()) };
                if result != 0 {
                    tracing::warn!("Cedar DMA unimport failed: {result}");
                }
            }
        }
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "aarch64")))]
mod imp {
    use super::*;

    pub fn available() -> bool {
        false
    }
    pub fn h265_available() -> bool { false }
    pub fn jpeg_available() -> bool { false }

    pub struct CedarH264;
    pub struct CedarDma;
    impl CedarDma {
        pub fn new(_: bool, _: u32, _: u32, _: u32, _: u32, _: u32, _: u32, _: u32, _: Vec<(OwnedFd, usize)>) -> Result<Self> {
            Err(AppError::VideoError("Cedar DMA requires Linux arm64".into()))
        }
        pub fn encode(&mut self, _: usize, _: i64, _: bool) -> Result<(Bytes, bool)> {
            Err(AppError::VideoError("Cedar DMA requires Linux arm64".into()))
        }
        pub fn set_bitrate(&mut self, _: u32) -> Result<()> {
            Err(AppError::VideoError("Cedar DMA requires Linux arm64".into()))
        }
    }
    impl CedarH264 {
        pub fn new(_: u32, _: u32, _: u32, _: u32, _: u32) -> Result<Self> {
            Err(AppError::VideoError("Cedar requires Linux arm64".into()))
        }
        pub fn new_jpeg(_: u32, _: u32, _: u32) -> Result<Self> {
            Err(AppError::VideoError("Cedar requires Linux arm64".into()))
        }
        pub fn new_scaled(_: u32, _: u32, _: u32, _: u32, _: u32, _: u32, _: u32) -> Result<Self> {
            Err(AppError::VideoError("Cedar requires Linux arm64".into()))
        }
        pub fn new_scaled_codec(_: bool, _: u32, _: u32, _: u32, _: u32, _: u32, _: u32, _: u32) -> Result<Self> {
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

pub use imp::{available, h265_available, jpeg_available, CedarDma, CedarH264};
