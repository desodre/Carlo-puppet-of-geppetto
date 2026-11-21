//! Public NDK media/EGL/GLES C ABI, pinned to the NDK 28.2.13676358 headers
//! `media/NdkMediaCodec.h`, `media/NdkMediaFormat.h`, `android/native_window.h`,
//! `EGL/egl.h`, `EGL/eglext.h` and `GLES2/gl2.h`. No private or C++ symbols.
//!
//! Exports are resolved at runtime so the binary still starts where a library is
//! absent. The table lives for the process: codec loopers and the EGL driver own
//! threads that may run library code after their handles are released, so the
//! libraries are never unloaded. Every handle has one RAII owner and is created,
//! used and released on the calling thread; the EGL context is current only there.
//! `eglSwapBuffers` and codec calls can block without a cancellation point: the
//! deadline is checked between calls and the bounded host session is the backstop.

use super::{Collector, Config, Error, Outcome, Sink};
use crate::binder::native::Library;
use std::{
    ffi::{CStr, c_char, c_void},
    ptr::{self, NonNull},
    sync::OnceLock,
    time::{Duration, Instant},
};

#[repr(C)]
struct Codec {
    _opaque: [u8; 0],
}
#[repr(C)]
struct Format {
    _opaque: [u8; 0],
}
/// Opaque `ANativeWindow`; only ever handled by pointer.
#[repr(C)]
pub(super) struct Window {
    _opaque: [u8; 0],
}
/// `AMediaCodecBufferInfo` from the pinned `NdkMediaCodec.h`.
#[repr(C)]
#[derive(Default)]
struct BufferInfo {
    offset: i32,
    size: i32,
    presentation_time_us: i64,
    flags: u32,
}
type Handle = *mut c_void;

const MIME: &CStr = c"video/avc";
const CONFIGURE_FLAG_ENCODE: u32 = 1;
/// `MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface`.
const COLOR_FORMAT_SURFACE: i32 = 0x7F00_0789;
const TRY_AGAIN_LATER: isize = -1;
const OUTPUT_FORMAT_CHANGED: isize = -2;
const OUTPUT_BUFFERS_CHANGED: isize = -3;
const DEADLINE: Duration = Duration::from_secs(10);
const DRAIN_SLICE_US: i64 = 10_000;

const EGL_ALPHA_SIZE: i32 = 0x3021;
const EGL_BLUE_SIZE: i32 = 0x3022;
const EGL_GREEN_SIZE: i32 = 0x3023;
const EGL_RED_SIZE: i32 = 0x3024;
const EGL_SURFACE_TYPE: i32 = 0x3033;
const EGL_NONE: i32 = 0x3038;
const EGL_RENDERABLE_TYPE: i32 = 0x3040;
const EGL_CONTEXT_CLIENT_VERSION: i32 = 0x3098;
const EGL_RECORDABLE_ANDROID: i32 = 0x3142;
const EGL_WINDOW_BIT: i32 = 4;
const EGL_OPENGL_ES2_BIT: i32 = 4;
const GL_SCISSOR_TEST: u32 = 0x0C11;
const GL_COLOR_BUFFER_BIT: u32 = 0x4000;

struct Api {
    _libraries: [Library; 4],
    codec_create: unsafe extern "C" fn(*const c_char) -> *mut Codec,
    codec_delete: unsafe extern "C" fn(*mut Codec) -> i32,
    codec_configure:
        unsafe extern "C" fn(*mut Codec, *const Format, *mut Window, *mut c_void, u32) -> i32,
    codec_input_surface: unsafe extern "C" fn(*mut Codec, *mut *mut Window) -> i32,
    codec_start: unsafe extern "C" fn(*mut Codec) -> i32,
    codec_stop: unsafe extern "C" fn(*mut Codec) -> i32,
    codec_end_input: unsafe extern "C" fn(*mut Codec) -> i32,
    codec_dequeue: unsafe extern "C" fn(*mut Codec, *mut BufferInfo, i64) -> isize,
    codec_buffer: unsafe extern "C" fn(*mut Codec, usize, *mut usize) -> *mut u8,
    codec_release_buffer: unsafe extern "C" fn(*mut Codec, usize, bool) -> i32,
    codec_output_format: unsafe extern "C" fn(*mut Codec) -> *mut Format,
    codec_name: unsafe extern "C" fn(*mut Codec, *mut *mut c_char) -> i32,
    codec_release_name: unsafe extern "C" fn(*mut Codec, *mut c_char),
    format_new: unsafe extern "C" fn() -> *mut Format,
    format_delete: unsafe extern "C" fn(*mut Format) -> i32,
    format_set_string: unsafe extern "C" fn(*mut Format, *const c_char, *const c_char),
    format_set_i32: unsafe extern "C" fn(*mut Format, *const c_char, i32),
    format_get_i32: unsafe extern "C" fn(*mut Format, *const c_char, *mut i32) -> bool,
    window_release: unsafe extern "C" fn(*mut Window),
    egl_display: unsafe extern "C" fn(Handle) -> Handle,
    egl_initialize: unsafe extern "C" fn(Handle, *mut i32, *mut i32) -> u32,
    egl_choose: unsafe extern "C" fn(Handle, *const i32, *mut Handle, i32, *mut i32) -> u32,
    egl_context: unsafe extern "C" fn(Handle, Handle, Handle, *const i32) -> Handle,
    egl_surface: unsafe extern "C" fn(Handle, Handle, *mut Window, *const i32) -> Handle,
    egl_current: unsafe extern "C" fn(Handle, Handle, Handle, Handle) -> u32,
    egl_time: unsafe extern "C" fn(Handle, Handle, i64) -> u32,
    egl_swap: unsafe extern "C" fn(Handle, Handle) -> u32,
    egl_destroy_surface: unsafe extern "C" fn(Handle, Handle) -> u32,
    egl_destroy_context: unsafe extern "C" fn(Handle, Handle) -> u32,
    egl_terminate: unsafe extern "C" fn(Handle) -> u32,
    egl_release_thread: unsafe extern "C" fn() -> u32,
    egl_error: unsafe extern "C" fn() -> i32,
    gl_viewport: unsafe extern "C" fn(i32, i32, i32, i32),
    gl_scissor: unsafe extern "C" fn(i32, i32, i32, i32),
    gl_enable: unsafe extern "C" fn(u32),
    gl_disable: unsafe extern "C" fn(u32),
    gl_clear_color: unsafe extern "C" fn(f32, f32, f32, f32),
    gl_clear: unsafe extern "C" fn(u32),
    gl_error: unsafe extern "C" fn() -> u32,
}

// SAFETY: immutable C function pointers plus owned dlopen handles that are never
// dropped (static table). Bionic permits calling resolved symbols from any thread;
// thread affinity of codec/EGL handles is enforced by their !Send owners below.
unsafe impl Send for Api {}
// SAFETY: same invariant as Send; the table has no mutable state.
unsafe impl Sync for Api {}

impl Api {
    fn global() -> Result<&'static Self, Error> {
        static API: OnceLock<Result<Api, Error>> = OnceLock::new();
        API.get_or_init(Self::load).as_ref().map_err(Clone::clone)
    }

    fn load() -> Result<Self, Error> {
        fn open(name: &'static CStr, label: &'static str) -> Result<Library, Error> {
            Library::open(name).ok_or(Error::LibraryUnavailable(label))
        }
        let media = open(c"libmediandk.so", "libmediandk.so")?;
        let window = open(c"libnativewindow.so", "libnativewindow.so")?;
        let egl = open(c"libEGL.so", "libEGL.so")?;
        let gles = open(c"libGLESv2.so", "libGLESv2.so")?;
        macro_rules! symbol {
            ($library:ident, $name:literal, $type:ty) => {{
                let address = $library.symbol($name).ok_or(Error::MissingSymbol(
                    $name.to_str().expect("static ASCII symbol"),
                ))?;
                // SAFETY: every invocation names the exact C signature of this public
                // export in the pinned NDK headers (media_status_t/EGLint/EGLBoolean
                // are 32-bit, ssize_t/size_t are pointer-sized, EGL handles are
                // pointers). The owning library stays loaded for the process.
                unsafe { std::mem::transmute::<*mut c_void, $type>(address.as_ptr()) }
            }};
        }
        Ok(Self {
            codec_create: symbol!(
                media,
                c"AMediaCodec_createEncoderByType",
                unsafe extern "C" fn(*const c_char) -> *mut Codec
            ),
            codec_delete: symbol!(
                media,
                c"AMediaCodec_delete",
                unsafe extern "C" fn(*mut Codec) -> i32
            ),
            codec_configure: symbol!(
                media,
                c"AMediaCodec_configure",
                unsafe extern "C" fn(
                    *mut Codec,
                    *const Format,
                    *mut Window,
                    *mut c_void,
                    u32,
                ) -> i32
            ),
            codec_input_surface: symbol!(
                media,
                c"AMediaCodec_createInputSurface",
                unsafe extern "C" fn(*mut Codec, *mut *mut Window) -> i32
            ),
            codec_start: symbol!(
                media,
                c"AMediaCodec_start",
                unsafe extern "C" fn(*mut Codec) -> i32
            ),
            codec_stop: symbol!(
                media,
                c"AMediaCodec_stop",
                unsafe extern "C" fn(*mut Codec) -> i32
            ),
            codec_end_input: symbol!(
                media,
                c"AMediaCodec_signalEndOfInputStream",
                unsafe extern "C" fn(*mut Codec) -> i32
            ),
            codec_dequeue: symbol!(
                media,
                c"AMediaCodec_dequeueOutputBuffer",
                unsafe extern "C" fn(*mut Codec, *mut BufferInfo, i64) -> isize
            ),
            codec_buffer: symbol!(
                media,
                c"AMediaCodec_getOutputBuffer",
                unsafe extern "C" fn(*mut Codec, usize, *mut usize) -> *mut u8
            ),
            codec_release_buffer: symbol!(
                media,
                c"AMediaCodec_releaseOutputBuffer",
                unsafe extern "C" fn(*mut Codec, usize, bool) -> i32
            ),
            codec_output_format: symbol!(
                media,
                c"AMediaCodec_getOutputFormat",
                unsafe extern "C" fn(*mut Codec) -> *mut Format
            ),
            codec_name: symbol!(
                media,
                c"AMediaCodec_getName",
                unsafe extern "C" fn(*mut Codec, *mut *mut c_char) -> i32
            ),
            codec_release_name: symbol!(
                media,
                c"AMediaCodec_releaseName",
                unsafe extern "C" fn(*mut Codec, *mut c_char)
            ),
            format_new: symbol!(
                media,
                c"AMediaFormat_new",
                unsafe extern "C" fn() -> *mut Format
            ),
            format_delete: symbol!(
                media,
                c"AMediaFormat_delete",
                unsafe extern "C" fn(*mut Format) -> i32
            ),
            format_set_string: symbol!(
                media,
                c"AMediaFormat_setString",
                unsafe extern "C" fn(*mut Format, *const c_char, *const c_char)
            ),
            format_set_i32: symbol!(
                media,
                c"AMediaFormat_setInt32",
                unsafe extern "C" fn(*mut Format, *const c_char, i32)
            ),
            format_get_i32: symbol!(
                media,
                c"AMediaFormat_getInt32",
                unsafe extern "C" fn(*mut Format, *const c_char, *mut i32) -> bool
            ),
            window_release: symbol!(
                window,
                c"ANativeWindow_release",
                unsafe extern "C" fn(*mut Window)
            ),
            egl_display: symbol!(
                egl,
                c"eglGetDisplay",
                unsafe extern "C" fn(Handle) -> Handle
            ),
            egl_initialize: symbol!(
                egl,
                c"eglInitialize",
                unsafe extern "C" fn(Handle, *mut i32, *mut i32) -> u32
            ),
            egl_choose: symbol!(
                egl,
                c"eglChooseConfig",
                unsafe extern "C" fn(Handle, *const i32, *mut Handle, i32, *mut i32) -> u32
            ),
            egl_context: symbol!(
                egl,
                c"eglCreateContext",
                unsafe extern "C" fn(Handle, Handle, Handle, *const i32) -> Handle
            ),
            egl_surface: symbol!(
                egl,
                c"eglCreateWindowSurface",
                unsafe extern "C" fn(Handle, Handle, *mut Window, *const i32) -> Handle
            ),
            egl_current: symbol!(
                egl,
                c"eglMakeCurrent",
                unsafe extern "C" fn(Handle, Handle, Handle, Handle) -> u32
            ),
            egl_time: symbol!(
                egl,
                c"eglPresentationTimeANDROID",
                unsafe extern "C" fn(Handle, Handle, i64) -> u32
            ),
            egl_swap: symbol!(
                egl,
                c"eglSwapBuffers",
                unsafe extern "C" fn(Handle, Handle) -> u32
            ),
            egl_destroy_surface: symbol!(
                egl,
                c"eglDestroySurface",
                unsafe extern "C" fn(Handle, Handle) -> u32
            ),
            egl_destroy_context: symbol!(
                egl,
                c"eglDestroyContext",
                unsafe extern "C" fn(Handle, Handle) -> u32
            ),
            egl_terminate: symbol!(egl, c"eglTerminate", unsafe extern "C" fn(Handle) -> u32),
            egl_release_thread: symbol!(egl, c"eglReleaseThread", unsafe extern "C" fn() -> u32),
            egl_error: symbol!(egl, c"eglGetError", unsafe extern "C" fn() -> i32),
            gl_viewport: symbol!(
                gles,
                c"glViewport",
                unsafe extern "C" fn(i32, i32, i32, i32)
            ),
            gl_scissor: symbol!(gles, c"glScissor", unsafe extern "C" fn(i32, i32, i32, i32)),
            gl_enable: symbol!(gles, c"glEnable", unsafe extern "C" fn(u32)),
            gl_disable: symbol!(gles, c"glDisable", unsafe extern "C" fn(u32)),
            gl_clear_color: symbol!(
                gles,
                c"glClearColor",
                unsafe extern "C" fn(f32, f32, f32, f32)
            ),
            gl_clear: symbol!(gles, c"glClear", unsafe extern "C" fn(u32)),
            gl_error: symbol!(gles, c"glGetError", unsafe extern "C" fn() -> u32),
            _libraries: [media, window, egl, gles],
        })
    }
}

fn status(operation: &'static str, status: i32) -> Result<(), Error> {
    if status == 0 {
        Ok(())
    } else {
        Err(Error::Codec { operation, status })
    }
}

/// Owns one `AMediaFormat`.
struct MediaFormat {
    api: &'static Api,
    raw: NonNull<Format>,
}

impl MediaFormat {
    fn encoder(api: &'static Api, config: &Config) -> Result<Self, Error> {
        // SAFETY: no arguments; a non-null result transfers one format to this owner.
        let raw = NonNull::new(unsafe { (api.format_new)() }).ok_or(Error::Codec {
            operation: "format_new",
            status: 0,
        })?;
        let format = Self { api, raw };
        // SAFETY: live owned format; key and value are terminated statics copied by the call.
        unsafe { (api.format_set_string)(raw.as_ptr(), c"mime".as_ptr(), MIME.as_ptr()) };
        for (key, value) in [
            (c"width", config.width),
            (c"height", config.height),
            (c"bitrate", config.bits_per_second),
            (c"frame-rate", config.frames_per_second),
            (c"i-frame-interval", config.key_frame_interval_seconds),
            (c"color-format", COLOR_FORMAT_SURFACE),
        ] {
            // SAFETY: live owned format and a terminated static key copied by the call.
            unsafe { (api.format_set_i32)(raw.as_ptr(), key.as_ptr(), value) };
        }
        Ok(format)
    }

    fn int(&self, key: &'static CStr) -> Option<i32> {
        let mut value = 0;
        // SAFETY: live owned format, terminated static key, valid out pointer for the call.
        unsafe { (self.api.format_get_i32)(self.raw.as_ptr(), key.as_ptr(), &raw mut value) }
            .then_some(value)
    }
}

impl Drop for MediaFormat {
    fn drop(&mut self) {
        // SAFETY: sole owner of a format from AMediaFormat_new/getOutputFormat, deleted once.
        unsafe { (self.api.format_delete)(self.raw.as_ptr()) };
    }
}

/// Owns one `AMediaCodec`; stops it before deletion when it was started.
struct Encoder {
    api: &'static Api,
    raw: NonNull<Codec>,
    started: bool,
}

impl Encoder {
    fn create(api: &'static Api) -> Result<Self, Error> {
        // SAFETY: terminated static MIME; null or one owned codec is returned.
        let raw = NonNull::new(unsafe { (api.codec_create)(MIME.as_ptr()) })
            .ok_or(Error::EncoderUnavailable)?;
        Ok(Self {
            api,
            raw,
            started: false,
        })
    }

    fn configure(&self, format: &MediaFormat) -> Result<(), Error> {
        // SAFETY: live codec and format; null surface/crypto are documented for a
        // Surface-input encoder. The format is only read during the call.
        let result = unsafe {
            (self.api.codec_configure)(
                self.raw.as_ptr(),
                format.raw.as_ptr(),
                ptr::null_mut(),
                ptr::null_mut(),
                CONFIGURE_FLAG_ENCODE,
            )
        };
        status("configure", result)
    }

    fn input_surface(&self) -> Result<InputWindow, Error> {
        let mut window = ptr::null_mut();
        // SAFETY: configured, not yet started codec; on success the out pointer
        // receives one window reference that the returned owner releases.
        let result = unsafe { (self.api.codec_input_surface)(self.raw.as_ptr(), &raw mut window) };
        status("create_input_surface", result)?;
        NonNull::new(window)
            .map(|raw| InputWindow { api: self.api, raw })
            .ok_or(Error::Codec {
                operation: "create_input_surface",
                status: 0,
            })
    }

    fn start(&mut self) -> Result<(), Error> {
        // SAFETY: live configured codec owned by this thread.
        status("start", unsafe {
            (self.api.codec_start)(self.raw.as_ptr())
        })?;
        self.started = true;
        Ok(())
    }

    fn name(&self) -> Option<String> {
        let mut name = ptr::null_mut();
        // SAFETY: live codec; on success name is a terminated string owned by the
        // codec API until releaseName, called below on the same codec.
        if unsafe { (self.api.codec_name)(self.raw.as_ptr(), &raw mut name) } != 0 || name.is_null()
        {
            return None;
        }
        // SAFETY: non-null terminated string valid until releaseName; copied first.
        let text = unsafe { CStr::from_ptr(name) }
            .to_str()
            .ok()
            .filter(|text| text.len() <= 128)
            .map(str::to_owned);
        // SAFETY: returns the exact pointer produced by getName on this codec, once.
        unsafe { (self.api.codec_release_name)(self.raw.as_ptr(), name) };
        text
    }

    fn end_input(&self) -> Result<(), Error> {
        // SAFETY: live started Surface-input codec.
        let result = unsafe { (self.api.codec_end_input)(self.raw.as_ptr()) };
        status("signal_end_of_input", result)
    }

    /// Move every currently available output buffer into `collector`, waiting at
    /// most `timeout_us` for the first. Each dequeued index is released exactly
    /// once, including when accounting rejects its contents.
    fn drain(
        &self,
        timeout_us: i64,
        collector: &mut dyn Sink,
        outcome: &mut Outcome,
    ) -> Result<(), Error> {
        let mut timeout_us = timeout_us;
        while !collector.ended() {
            let mut info = BufferInfo::default();
            // SAFETY: live started codec; info is a valid out pointer for the call.
            let index =
                unsafe { (self.api.codec_dequeue)(self.raw.as_ptr(), &raw mut info, timeout_us) };
            timeout_us = 0;
            match index {
                TRY_AGAIN_LATER => return Ok(()),
                OUTPUT_BUFFERS_CHANGED => {}
                OUTPUT_FORMAT_CHANGED => {
                    // SAFETY: live codec; a non-null result is a new format owned here.
                    let raw = unsafe { (self.api.codec_output_format)(self.raw.as_ptr()) };
                    if let Some(raw) = NonNull::new(raw) {
                        let format = MediaFormat { api: self.api, raw };
                        outcome.output_width = format.int(c"width");
                        outcome.output_height = format.int(c"height");
                        collector.format(outcome.output_width, outcome.output_height);
                    }
                }
                index if index < 0 => {
                    return Err(Error::Codec {
                        operation: "dequeue_output",
                        status: i32::try_from(index).unwrap_or(i32::MIN),
                    });
                }
                index => {
                    let index = index.unsigned_abs();
                    let accounted = self.account(index, &info, collector);
                    // SAFETY: index came from dequeueOutputBuffer on this codec and
                    // no slice into its buffer outlives `account`.
                    let released =
                        unsafe { (self.api.codec_release_buffer)(self.raw.as_ptr(), index, false) };
                    accounted?;
                    status("release_output", released)?;
                }
            }
        }
        Ok(())
    }

    fn account(
        &self,
        index: usize,
        info: &BufferInfo,
        collector: &mut dyn Sink,
    ) -> Result<(), Error> {
        let invalid = Error::InvalidStream("buffer_range");
        let offset = usize::try_from(info.offset).map_err(|_| invalid.clone())?;
        let size = usize::try_from(info.size).map_err(|_| invalid.clone())?;
        if size == 0 {
            return collector.push(info.flags, info.presentation_time_us, &[]);
        }
        let mut capacity = 0;
        // SAFETY: index is a dequeued, unreleased output buffer of this live codec;
        // capacity is a valid out pointer for the call.
        let buffer =
            unsafe { (self.api.codec_buffer)(self.raw.as_ptr(), index, &raw mut capacity) };
        let end = offset.checked_add(size).ok_or_else(|| invalid.clone())?;
        if buffer.is_null() || end > capacity {
            return Err(invalid);
        }
        // SAFETY: the codec guarantees `capacity` readable bytes at `buffer` until the
        // index is released; [offset, end) was bounds-checked above, the encoder does
        // not write this dequeued buffer, and the slice ends before release in `drain`.
        let data = unsafe { std::slice::from_raw_parts(buffer.add(offset), size) };
        collector.push(info.flags, info.presentation_time_us, data)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        if self.started {
            // SAFETY: live started codec; its input window/EGL surface owners are
            // declared after it and therefore already dropped.
            unsafe { (self.api.codec_stop)(self.raw.as_ptr()) };
        }
        // SAFETY: sole owner of the codec from createEncoderByType, deleted once.
        unsafe { (self.api.codec_delete)(self.raw.as_ptr()) };
    }
}

/// Owns the encoder input `ANativeWindow` reference from createInputSurface.
struct InputWindow {
    api: &'static Api,
    raw: NonNull<Window>,
}

impl Drop for InputWindow {
    fn drop(&mut self) {
        // SAFETY: releases the single reference transferred by createInputSurface;
        // the EGL surface borrowing it is declared later and already destroyed.
        unsafe { (self.api.window_release)(self.raw.as_ptr()) };
    }
}

/// A started encoder that is fed no frames, with its input window, for proofs that
/// need the surface only. It must be started: on the executed profile a codec that
/// is deleted after `createInputSurface` without `start` leaves one platform
/// `C2NodeImpl` thread behind per session. Fields drop in order: window, then codec.
pub(super) struct ConfiguredInput {
    window: InputWindow,
    encoder: Encoder,
}

impl ConfiguredInput {
    pub(super) fn new(config: &Config) -> Result<Self, Error> {
        let api = Api::global()?;
        let mut encoder = Encoder::create(api)?;
        encoder.configure(&MediaFormat::encoder(api, config)?)?;
        let window = encoder.input_surface()?;
        encoder.start()?;
        Ok(Self { window, encoder })
    }

    pub(super) fn codec_name(&self) -> Option<String> {
        self.encoder.name()
    }

    /// The input window, borrowed: valid while `self` lives and never released
    /// by the caller.
    pub(super) fn window(&self) -> NonNull<Window> {
        self.window.raw
    }

    /// Move the currently available output into the sink, waiting at most
    /// `timeout_us` for the first buffer. Frames come from whoever queues to the
    /// input window; this type never draws.
    pub(super) fn drain(
        &self,
        timeout_us: i64,
        collector: &mut dyn Sink,
        outcome: &mut Outcome,
    ) -> Result<(), Error> {
        self.encoder.drain(timeout_us, collector, outcome)
    }

    /// Ask the encoder to finish; end of stream then arrives through `drain`.
    pub(super) fn end_input(&self) -> Result<(), Error> {
        self.encoder.end_input()
    }
}

/// Owns display initialization, one ES2 context and one window surface, current
/// on the creating thread. `NonNull`/raw handles keep it `!Send`.
struct Renderer<'a> {
    api: &'static Api,
    display: NonNull<c_void>,
    context: Handle,
    surface: Handle,
    _window: &'a InputWindow,
}

impl<'a> Renderer<'a> {
    fn new(api: &'static Api, window: &'a InputWindow) -> Result<Self, Error> {
        let failure = |operation| Error::Egl {
            operation,
            // SAFETY: eglGetError only reads this thread's EGL error state.
            code: unsafe { (api.egl_error)() },
        };
        // SAFETY: EGL_DEFAULT_DISPLAY is the null native display.
        let display = NonNull::new(unsafe { (api.egl_display)(ptr::null_mut()) })
            .ok_or_else(|| failure("get_display"))?;
        // SAFETY: valid display; null version out pointers are permitted by EGL 1.4.
        if unsafe { (api.egl_initialize)(display.as_ptr(), ptr::null_mut(), ptr::null_mut()) } == 0
        {
            return Err(failure("initialize"));
        }
        // From here Drop terminates the display and releases whatever exists.
        let mut renderer = Self {
            api,
            display,
            context: ptr::null_mut(),
            surface: ptr::null_mut(),
            _window: window,
        };
        let attributes = [
            EGL_RED_SIZE,
            8,
            EGL_GREEN_SIZE,
            8,
            EGL_BLUE_SIZE,
            8,
            EGL_ALPHA_SIZE,
            8,
            EGL_RENDERABLE_TYPE,
            EGL_OPENGL_ES2_BIT,
            EGL_SURFACE_TYPE,
            EGL_WINDOW_BIT,
            EGL_RECORDABLE_ANDROID,
            1,
            EGL_NONE,
        ];
        let mut config = ptr::null_mut();
        let mut count = 0;
        // SAFETY: initialized display; EGL_NONE-terminated attributes and out
        // pointers for exactly one config are valid for the call.
        let chosen = unsafe {
            (api.egl_choose)(
                display.as_ptr(),
                attributes.as_ptr(),
                &raw mut config,
                1,
                &raw mut count,
            )
        };
        if chosen == 0 || count < 1 {
            return Err(failure("choose_config"));
        }
        let context_attributes = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
        // SAFETY: initialized display, config returned above, no share context and
        // an EGL_NONE-terminated attribute list.
        renderer.context = unsafe {
            (api.egl_context)(
                display.as_ptr(),
                config,
                ptr::null_mut(),
                context_attributes.as_ptr(),
            )
        };
        if renderer.context.is_null() {
            return Err(failure("create_context"));
        }
        // SAFETY: the borrowed window outlives this renderer; EGL takes its own
        // reference for the surface. A null attribute list is permitted.
        renderer.surface = unsafe {
            (api.egl_surface)(display.as_ptr(), config, window.raw.as_ptr(), ptr::null())
        };
        if renderer.surface.is_null() {
            return Err(failure("create_window_surface"));
        }
        // SAFETY: display, surface and context were created above on this thread.
        let current = unsafe {
            (api.egl_current)(
                display.as_ptr(),
                renderer.surface,
                renderer.surface,
                renderer.context,
            )
        };
        if current == 0 {
            return Err(failure("make_current"));
        }
        Ok(renderer)
    }

    /// Draw one frame (cycling background, moving bar) and queue it with `pts_ns`.
    fn frame(&self, config: &Config, index: u32, pts_ns: i64) -> Result<(), Error> {
        let api = self.api;
        let phase = index as f32 / config.frames.max(1) as f32;
        let bar_width = (config.width / 8).max(1);
        let bar_x = ((config.width - bar_width) as f32 * phase) as i32;
        // SAFETY: this renderer's ES2 context is current on this thread; arguments
        // are plain values within the surface dimensions.
        let error = unsafe {
            (api.gl_viewport)(0, 0, config.width, config.height);
            (api.gl_disable)(GL_SCISSOR_TEST);
            (api.gl_clear_color)(phase, 0.25, 1.0 - phase, 1.0);
            (api.gl_clear)(GL_COLOR_BUFFER_BIT);
            (api.gl_enable)(GL_SCISSOR_TEST);
            (api.gl_scissor)(bar_x, 0, bar_width, config.height);
            (api.gl_clear_color)(1.0, 1.0, 1.0, 1.0);
            (api.gl_clear)(GL_COLOR_BUFFER_BIT);
            (api.gl_disable)(GL_SCISSOR_TEST);
            (api.gl_error)()
        };
        if error != 0 {
            return Err(Error::Gl { code: error });
        }
        let failure = |operation| Error::Egl {
            operation,
            // SAFETY: eglGetError only reads this thread's EGL error state.
            code: unsafe { (api.egl_error)() },
        };
        // SAFETY: live display and current window surface owned by this renderer.
        if unsafe { (api.egl_time)(self.display.as_ptr(), self.surface, pts_ns) } == 0 {
            return Err(failure("presentation_time"));
        }
        // SAFETY: same handles; queues the drawn buffer to the encoder input.
        if unsafe { (api.egl_swap)(self.display.as_ptr(), self.surface) } == 0 {
            return Err(failure("swap_buffers"));
        }
        Ok(())
    }
}

impl Drop for Renderer<'_> {
    fn drop(&mut self) {
        let display = self.display.as_ptr();
        // SAFETY: handles were created by this owner on this thread and are
        // destroyed once; unbinding first makes destruction immediate. Terminate
        // and release-thread drop the display/thread state this probe initialized.
        unsafe {
            (self.api.egl_current)(display, ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
            if !self.surface.is_null() {
                (self.api.egl_destroy_surface)(display, self.surface);
            }
            if !self.context.is_null() {
                (self.api.egl_destroy_context)(display, self.context);
            }
            (self.api.egl_terminate)(display);
            (self.api.egl_release_thread)();
        }
    }
}

/// Encode `config.frames` synthetic frames and drain them into `collector`.
///
/// Locals drop in reverse order: renderer (EGL surface), input window, encoder.
pub(super) fn encode(
    config: &Config,
    collector: &mut Collector,
    outcome: &mut Outcome,
) -> Result<(), Error> {
    let api = Api::global()?;
    let deadline = Instant::now() + DEADLINE;
    let mut encoder = Encoder::create(api)?;
    encoder.configure(&MediaFormat::encoder(api, config)?)?;
    let window = encoder.input_surface()?;
    encoder.start()?;
    outcome.codec_name = encoder.name();
    let renderer = Renderer::new(api, &window)?;
    let frame_ns = 1_000_000_000 / i64::from(config.frames_per_second);
    for index in 0..config.frames {
        if Instant::now() >= deadline {
            return Err(Error::Timeout("render"));
        }
        renderer.frame(config, index, i64::from(index) * frame_ns)?;
        outcome.frames_submitted += 1;
        encoder.drain(0, collector, outcome)?;
    }
    encoder.end_input()?;
    while !collector.ended() {
        if Instant::now() >= deadline {
            return Err(Error::Timeout("drain"));
        }
        encoder.drain(DRAIN_SLICE_US, collector, outcome)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::PRESET;

    /// Open descriptors and threads. Platform codec teardown is asynchronous, so
    /// the lowest of a few spaced samples is used: a leak persists, a transient
    /// descriptor does not.
    fn resources() -> (usize, usize) {
        (0..5)
            .map(|_| {
                std::thread::sleep(Duration::from_millis(100));
                (
                    std::fs::read_dir("/proc/self/fd").unwrap().count(),
                    std::fs::read_dir("/proc/self/task").unwrap().count(),
                )
            })
            .min()
            .unwrap()
    }

    fn run(config: &Config) -> (Result<(), Error>, Collector, Outcome) {
        let mut collector = Collector::new(config, false);
        let mut outcome = Outcome::default();
        let result = encode(config, &mut collector, &mut outcome);
        (result, collector, outcome)
    }

    fn successful() {
        let (result, collector, outcome) = run(&PRESET);
        assert_eq!(result, Ok(()), "{outcome:?}");
        assert!(collector.ended());
        assert_eq!(outcome.frames_submitted, PRESET.frames);
        assert_eq!(collector.stream.coded_frames(), PRESET.frames);
        assert!(collector.stream.nal_units.sps >= 1 && collector.stream.nal_units.pps >= 1);
        assert_eq!(
            (outcome.output_width, outcome.output_height),
            (Some(PRESET.width), Some(PRESET.height))
        );
    }

    /// The platform media client keeps two ashmem descriptors per codec session in a
    /// cache it evicts, on the next codec activity, once they are about five seconds
    /// old (observed on the executed profile). Carlo's own handles are all released
    /// at drop, so the count must return to its floor after one idle period and may
    /// only grow by that cached pair per session in between.
    const PLATFORM_CACHE_IDLE: Duration = Duration::from_secs(6);
    const PLATFORM_CACHED_FDS_PER_SESSION: usize = 2;

    fn settled() -> (usize, usize) {
        std::thread::sleep(PLATFORM_CACHE_IDLE);
        successful();
        resources()
    }

    #[test]
    fn android_encoder_cycles_release_resources_and_rejected_format_is_typed() {
        let environment = crate::diagnostics::Diagnostics::collect().unwrap();
        assert!(crate::video::executed(
            environment.shell_api(),
            std::env::consts::ARCH
        ));
        // Libraries, driver state and service connections are created lazily once.
        successful();
        successful();
        let floor = settled();
        let mut burst = Vec::new();
        for cycle in 1..=5 {
            successful();
            let sample = resources();
            assert!(
                sample.0 <= floor.0 + cycle * PLATFORM_CACHED_FDS_PER_SESSION,
                "{floor:?} {burst:?} {sample:?}"
            );
            burst.push(sample);
        }
        // A format the encoder cannot accept fails in a named codec call, produces
        // no output and releases the codec it had created without new descriptors.
        let before_rejection = resources();
        let (result, collector, outcome) = run(&Config {
            width: 0,
            height: 0,
            ..PRESET
        });
        assert!(
            matches!(
                result,
                Err(Error::Codec {
                    operation: "configure" | "create_input_surface" | "start",
                    status
                }) if status != 0
            ),
            "{result:?}"
        );
        assert_eq!(collector.stream.packets, 0);
        assert_eq!(outcome.frames_submitted, 0);
        let after_rejection = resources();
        assert!(after_rejection.0 <= before_rejection.0);
        let recovered = [settled(), settled()];
        println!(
            "CARLO_VIDEO_RESOURCE_CYCLES {}",
            serde_json::json!({
                "floor": floor,
                "burst": burst,
                "after_rejected_format": after_rejection,
                "after_idle": recovered,
            })
        );
        // Codec looper threads are transient; a per-session leak would exceed this slack.
        for sample in burst.iter().chain(&recovered) {
            assert!(sample.1 <= floor.1 + 4, "{floor:?} {burst:?} {recovered:?}");
        }
        for sample in recovered {
            assert!(sample.0 <= floor.0, "{floor:?} {burst:?} {recovered:?}");
        }
    }

    fn flattened() {
        let outcome = crate::video::surface::probe();
        assert!(outcome.passed(), "{outcome:?}");
        assert!(outcome.magic_matches && outcome.producer_present);
        assert_eq!(outcome.error, None);
    }

    /// The three provoked failures, once, through the fault command's own profile
    /// gate: each must report exactly its class, none may leave a display, and the
    /// process must return to the resource floor. This is the first test in name
    /// order (`--test-threads=1`), so its first session starts the Binder thread
    /// pool before any codec use of the process, as the mirror path requires.
    #[test]
    fn android_display_fault_sessions_are_classified_and_release_resources() {
        let report = crate::video::mirror::collect_faults().unwrap();
        let json = serde_json::to_value(&report).unwrap();
        let sessions = &json["sessions"];
        assert!(report.passed(), "{sessions}");
        for (index, class) in ["permission_denied", "display_absent", "unsupported_format"]
            .into_iter()
            .enumerate()
        {
            let outcome = &sessions[index]["outcome"];
            assert_eq!(outcome["failure_class"], class, "{sessions}");
            assert_eq!(outcome["display_created"], false, "{sessions}");
            assert!(outcome["release_error"].is_null(), "{sessions}");
        }
        assert_eq!(sessions[1]["outcome"]["transactions"]["create"], 0);
        assert_eq!(
            sessions[2]["outcome"]["transactions"],
            serde_json::json!({"list": 0, "create": 0, "release": 0})
        );
        let after = resources();
        let floor = settled();
        let recovered = [settled(), settled()];
        println!(
            "CARLO_DISPLAY_FAULT_RESOURCE_CYCLES {}",
            serde_json::json!({"after": after, "floor": floor, "after_idle": recovered})
        );
        for sample in recovered {
            assert!(sample.0 <= floor.0, "{after:?} {floor:?} {recovered:?}");
            assert!(sample.1 <= floor.1 + 4, "{after:?} {floor:?} {recovered:?}");
        }
    }

    /// One real create/mirror/release cycle through the command's own profile gate
    /// and release guard. The pool must be started by this path before a codec
    /// could start a foreign one (tests run in name order with `--test-threads=1`;
    /// the fault test above does that first): the mirror path owns the
    /// Binder thread pool and refuses one that already exists. Needs the screen on;
    /// an idle screen may yield a single frame, which is enough. The pool workers
    /// and lazily opened drivers it leaves are part of the floor measured after it.
    #[test]
    fn android_display_mirror_cycle_creates_releases_and_settles() {
        let report = crate::video::mirror::collect(false).unwrap();
        let outcome = report.outcome();
        assert!(report.passed(), "{outcome:?}");
        assert!(outcome.display_created && outcome.display_released);
        assert_eq!(outcome.error, None, "{outcome:?}");
        let after = resources();
        let floor = settled();
        let recovered = [settled(), settled()];
        println!(
            "CARLO_DISPLAY_MIRROR_RESOURCE_CYCLES {}",
            serde_json::json!({"after": after, "floor": floor, "after_idle": recovered})
        );
        for sample in recovered {
            assert!(sample.0 <= floor.0, "{after:?} {floor:?} {recovered:?}");
            assert!(sample.1 <= floor.1 + 4, "{after:?} {floor:?} {recovered:?}");
        }
    }

    /// Each cycle starts a codec session that encodes nothing, so the same cached
    /// descriptor pair is tolerated per cycle; `settled` measures the floor with a
    /// full encode, the activity known to evict that cache.
    #[test]
    fn android_surface_parcel_cycles_expose_producer_and_release_resources() {
        const CYCLES: usize = 10;
        let environment = crate::diagnostics::Diagnostics::collect().unwrap();
        assert!(crate::video::executed(
            environment.shell_api(),
            std::env::consts::ARCH
        ));
        // Libraries, the Binder driver handle and service connections are lazy.
        successful();
        flattened();
        flattened();
        let floor = settled();
        let mut burst = Vec::new();
        for cycle in 1..=CYCLES {
            flattened();
            let sample = resources();
            assert!(
                sample.0 <= floor.0 + cycle * PLATFORM_CACHED_FDS_PER_SESSION,
                "{floor:?} {burst:?} {sample:?}"
            );
            burst.push(sample);
        }
        let recovered = [settled(), settled()];
        println!(
            "CARLO_SURFACE_PARCEL_RESOURCE_CYCLES {}",
            serde_json::json!({"floor": floor, "burst": burst, "after_idle": recovered})
        );
        for sample in burst.iter().chain(&recovered) {
            assert!(sample.1 <= floor.1 + 4, "{floor:?} {burst:?} {recovered:?}");
        }
        for sample in recovered {
            assert!(sample.0 <= floor.0, "{floor:?} {burst:?} {recovered:?}");
        }
    }
}
