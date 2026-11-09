//! libmpv, loaded when the first player is made (dlopen), so the core builds and starts
//! without it, and only apps that play something pay for it. The functions used are a
//! small, stable part of mpv's C API (client.h, API version 2), declared here by hand.
//!
//! Which library: the file named by ELSEWHERE_LIBMPV (desktop development), else the
//! platform's usual name (libmpv.so on Android and Linux, mpv.framework on iOS,
//! libmpv.2.dylib on macOS).

use std::ffi::{CStr, CString, c_char, c_int, c_ulong, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::machine::{EndReason, Event};

type Ctx = *mut c_void;

#[cfg(any(target_os = "linux", target_os = "android"))]
#[link(name = "dl")]
unsafe extern "C" {}

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *mut c_char;
}

const RTLD_NOW: c_int = 2;

const FORMAT_STRING: c_int = 1;
const FORMAT_FLAG: c_int = 3;
const FORMAT_INT64: c_int = 4;
const FORMAT_DOUBLE: c_int = 5;

const EVENT_NONE: c_int = 0;
const EVENT_SHUTDOWN: c_int = 1;
const EVENT_LOG_MESSAGE: c_int = 2;
const EVENT_START_FILE: c_int = 6;
const EVENT_END_FILE: c_int = 7;
const EVENT_FILE_LOADED: c_int = 8;
const EVENT_PROPERTY_CHANGE: c_int = 22;
const EVENT_QUEUE_OVERFLOW: c_int = 24;

/// mpv's error code for an option this mpv version does not have.
pub(super) const OPTION_NOT_FOUND: c_int = -5;

#[repr(C)]
struct RawEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct RawProperty {
    name: *const c_char,
    format: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct RawStartFile {
    playlist_entry_id: i64,
}

#[repr(C)]
struct RawEndFile {
    reason: c_int,
    error: c_int,
    playlist_entry_id: i64,
    playlist_insert_id: i64,
    playlist_insert_num_entries: c_int,
}

#[repr(C)]
struct RawLogMessage {
    prefix: *const c_char,
    level: *const c_char,
    text: *const c_char,
    log_level: c_int,
}

type Version = unsafe extern "C" fn() -> c_ulong;
type Create = unsafe extern "C" fn() -> Ctx;
type WithCtx = unsafe extern "C" fn(Ctx);
type Status = unsafe extern "C" fn(Ctx) -> c_int;
type SetOption = unsafe extern "C" fn(Ctx, *const c_char, *const c_char) -> c_int;
type Property = unsafe extern "C" fn(Ctx, *const c_char, c_int, *mut c_void) -> c_int;
type Command = unsafe extern "C" fn(Ctx, *const *const c_char) -> c_int;
type Observe = unsafe extern "C" fn(Ctx, u64, *const c_char, c_int) -> c_int;
type Logs = unsafe extern "C" fn(Ctx, *const c_char) -> c_int;
type Wait = unsafe extern "C" fn(Ctx, f64) -> *mut RawEvent;
type ErrorString = unsafe extern "C" fn(c_int) -> *const c_char;
type Free = unsafe extern "C" fn(*mut c_void);
type RenderCreate = unsafe extern "C" fn(*mut *mut c_void, Ctx, *mut RenderParam) -> c_int;
type RenderCallback = unsafe extern "C" fn(*mut c_void);
type RenderSetCallback = unsafe extern "C" fn(*mut c_void, Option<RenderCallback>, *mut c_void);
type RenderUpdate = unsafe extern "C" fn(*mut c_void) -> u64;
type RenderRender = unsafe extern "C" fn(*mut c_void, *mut RenderParam) -> c_int;
type RenderFree = unsafe extern "C" fn(*mut c_void);

/// render.h: one parameter of the render API (type, then a pointer to its value).
#[repr(C)]
struct RenderParam {
    kind: c_int,
    data: *mut c_void,
}

const RENDER_PARAM_INVALID: c_int = 0;
const RENDER_PARAM_API_TYPE: c_int = 1;
const RENDER_PARAM_SW_SIZE: c_int = 17;
const RENDER_PARAM_SW_FORMAT: c_int = 18;
const RENDER_PARAM_SW_STRIDE: c_int = 19;
const RENDER_PARAM_SW_POINTER: c_int = 20;
const RENDER_UPDATE_FRAME: u64 = 1;

/// The library's functions.
struct Api {
    client_api_version: Version,
    create: Create,
    initialize: Status,
    terminate_destroy: WithCtx,
    set_option_string: SetOption,
    set_property: Property,
    get_property: Property,
    command: Command,
    observe_property: Observe,
    request_log_messages: Logs,
    wait_event: Wait,
    wakeup: WithCtx,
    error_string: ErrorString,
    free: Free,
    render_create: RenderCreate,
    render_set_callback: RenderSetCallback,
    render_update: RenderUpdate,
    render_render: RenderRender,
    render_free: RenderFree,
}

/// Only function pointers into a library that stays loaded.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn library_names() -> Vec<String> {
    let mut names: Vec<String> = std::env::var("ELSEWHERE_LIBMPV").into_iter().collect();
    let platform: &[&str] = if cfg!(target_os = "ios") {
        // Embedded by the app (scripts/ios_mpv.sh); test programs have it next to them.
        &["@rpath/mpv.framework/mpv"]
    } else if cfg!(target_os = "macos") {
        &[
            "libmpv.2.dylib",
            "libmpv.dylib",
            "/opt/homebrew/lib/libmpv.dylib",
        ]
    } else if cfg!(target_os = "android") {
        &["libmpv.so"]
    } else {
        &["libmpv.so.2", "libmpv.so"]
    };
    names.extend(platform.iter().map(|name| (*name).to_owned()));
    names
}

fn load(names: Vec<String>) -> Result<Api, String> {
    let mut tried = Vec::new();
    let library = names.into_iter().find_map(|name| {
        let path = CString::new(name.clone()).ok()?;
        // SAFETY: dlopen with a valid C string; a null result is handled.
        let library = unsafe { dlopen(path.as_ptr(), RTLD_NOW) };
        if library.is_null() {
            // SAFETY: dlerror returns null or a C string valid until the next dl call.
            let error = unsafe { text(dlerror()) }.unwrap_or_default();
            tried.push(format!("{name}: {error}"));
            None
        } else {
            Some(library)
        }
    });
    let Some(library) = library else {
        return Err(format!("libmpv is not there ({})", tried.join("; ")));
    };
    macro_rules! symbol {
        ($name:literal, $kind:ty) => {{
            // SAFETY: the name is a C string; a null result is handled below.
            let found = unsafe { dlsym(library, concat!($name, "\0").as_ptr().cast()) };
            if found.is_null() {
                return Err(format!("libmpv has no {}", $name));
            }
            // SAFETY: the symbol is that mpv function, and $kind is its C signature
            // (client.h, API version 2).
            unsafe { std::mem::transmute::<*mut c_void, $kind>(found) }
        }};
    }
    let api = Api {
        client_api_version: symbol!("mpv_client_api_version", Version),
        create: symbol!("mpv_create", Create),
        initialize: symbol!("mpv_initialize", Status),
        terminate_destroy: symbol!("mpv_terminate_destroy", WithCtx),
        set_option_string: symbol!("mpv_set_option_string", SetOption),
        set_property: symbol!("mpv_set_property", Property),
        get_property: symbol!("mpv_get_property", Property),
        command: symbol!("mpv_command", Command),
        observe_property: symbol!("mpv_observe_property", Observe),
        request_log_messages: symbol!("mpv_request_log_messages", Logs),
        wait_event: symbol!("mpv_wait_event", Wait),
        wakeup: symbol!("mpv_wakeup", WithCtx),
        error_string: symbol!("mpv_error_string", ErrorString),
        free: symbol!("mpv_free", Free),
        render_create: symbol!("mpv_render_context_create", RenderCreate),
        render_set_callback: symbol!("mpv_render_context_set_update_callback", RenderSetCallback),
        render_update: symbol!("mpv_render_context_update", RenderUpdate),
        render_render: symbol!("mpv_render_context_render", RenderRender),
        render_free: symbol!("mpv_render_context_free", RenderFree),
    };
    #[cfg(target_os = "android")]
    super::android::hand_vm(library);
    // SAFETY: a function without arguments.
    let version = unsafe { (api.client_api_version)() };
    if version >> 16 != 2 {
        return Err(format!(
            "libmpv has API version {}.{}; this core needs 2.x",
            version >> 16,
            version & 0xffff
        ));
    }
    Ok(api)
}

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(|| load(library_names()))
        .as_ref()
        .map_err(Clone::clone)
}

/// Copies a C string (null for none).
///
/// # Safety
/// `ptr` is null or a valid C string.
unsafe fn text(ptr: *const c_char) -> Option<String> {
    (!ptr.is_null()).then(|| {
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned()
    })
}

fn c(text: &str) -> CString {
    // Interior NUL bytes cannot reach mpv: they are cut off.
    CString::new(text.split('\0').next().unwrap_or_default()).unwrap_or_default()
}

/// Which property an observation reports (the reply_userdata of mpv_observe_property).
pub(super) const OBSERVED: [(&str, c_int); 8] = [
    ("time-pos", FORMAT_DOUBLE),
    ("duration", FORMAT_DOUBLE),
    ("pause", FORMAT_FLAG),
    ("eof-reached", FORMAT_FLAG),
    ("speed", FORMAT_DOUBLE),
    ("volume", FORMAT_DOUBLE),
    // The video output has a picture to show (video on, a video playing).
    ("vo-configured", FORMAT_FLAG),
    // Which hardware decoder decodes the video ("no": software).
    ("hwdec-current", FORMAT_STRING),
];

/// What `wait` got: a machine event, or something for the event loop itself.
pub(super) enum Waited {
    Nothing,
    Event(Event),
    /// A log line from mpv: its level ("error", "warn", ...) and text.
    Log(String, String),
    FileLoaded,
    Overflow,
    Shutdown,
}

/// One mpv instance. mpv's functions are thread-safe; only `wait` must run on one
/// thread at a time (the player's event thread).
pub(super) struct Handle {
    api: &'static Api,
    ctx: Ctx,
    /// Its player is gone and it is being freed (`closing`).
    closing: AtomicBool,
}

/// How many handles are being freed: a player frees mpv on its own thread after it is
/// dropped (`wait_closed`).
static CLOSING: Mutex<usize> = Mutex::new(0);
static CLOSED: Condvar = Condvar::new();

/// Waits until no handle is being freed, at most `timeout`; true if none is.
pub(super) fn wait_closed(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut closing = CLOSING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    while *closing > 0 {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        closing = CLOSED
            .wait_timeout(closing, left)
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .0;
    }
    true
}

unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    pub fn create() -> Result<Handle, String> {
        let api = api()?;
        // SAFETY: no arguments; null means out of memory.
        let ctx = unsafe { (api.create)() };
        if ctx.is_null() {
            return Err("mpv_create failed".to_owned());
        }
        Ok(Handle {
            api,
            ctx,
            closing: AtomicBool::new(false),
        })
    }

    /// Says the handle's player is gone and the handle is about to be freed: until it
    /// is, `wait_closed` waits for it.
    pub fn closing(&self) {
        if !self.closing.swap(true, Ordering::SeqCst) {
            *CLOSING
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
        }
    }

    pub fn error_text(&self, code: c_int) -> String {
        // SAFETY: mpv_error_string returns a static string for any code.
        let name = unsafe { text((self.api.error_string)(code)) }.unwrap_or_default();
        format!("{name} ({code})")
    }

    fn check(code: c_int) -> Result<(), c_int> {
        if code >= 0 { Ok(()) } else { Err(code) }
    }

    pub fn set_option(&self, name: &str, value: &str) -> Result<(), c_int> {
        let (name, value) = (c(name), c(value));
        // SAFETY: valid handle and C strings.
        Self::check(unsafe {
            (self.api.set_option_string)(self.ctx, name.as_ptr(), value.as_ptr())
        })
    }

    pub fn initialize(&self) -> Result<(), c_int> {
        // SAFETY: valid handle, before any other use but options.
        Self::check(unsafe { (self.api.initialize)(self.ctx) })
    }

    pub fn command(&self, args: &[&str]) -> Result<(), c_int> {
        let owned: Vec<CString> = args.iter().map(|arg| c(arg)).collect();
        let mut pointers: Vec<*const c_char> = owned.iter().map(|arg| arg.as_ptr()).collect();
        pointers.push(std::ptr::null());
        // SAFETY: a null-terminated array of C strings that outlive the call.
        Self::check(unsafe { (self.api.command)(self.ctx, pointers.as_ptr()) })
    }

    pub fn set_flag(&self, name: &str, value: bool) -> Result<(), c_int> {
        let name = c(name);
        let mut flag: c_int = value.into();
        // SAFETY: FLAG takes a pointer to an int.
        Self::check(unsafe {
            (self.api.set_property)(self.ctx, name.as_ptr(), FORMAT_FLAG, (&raw mut flag).cast())
        })
    }

    pub fn set_double(&self, name: &str, value: f64) -> Result<(), c_int> {
        let name = c(name);
        let mut number = value;
        // SAFETY: DOUBLE takes a pointer to a double.
        Self::check(unsafe {
            (self.api.set_property)(
                self.ctx,
                name.as_ptr(),
                FORMAT_DOUBLE,
                (&raw mut number).cast(),
            )
        })
    }

    pub fn set_string(&self, name: &str, value: &str) -> Result<(), c_int> {
        let (name, value) = (c(name), c(value));
        let mut pointer = value.as_ptr();
        // SAFETY: STRING takes a pointer to a char pointer; mpv copies the string.
        Self::check(unsafe {
            (self.api.set_property)(
                self.ctx,
                name.as_ptr(),
                FORMAT_STRING,
                (&raw mut pointer).cast(),
            )
        })
    }

    pub fn get_i64(&self, name: &str) -> Option<i64> {
        let name = c(name);
        let mut number: i64 = 0;
        // SAFETY: INT64 writes an int64_t.
        let code = unsafe {
            (self.api.get_property)(
                self.ctx,
                name.as_ptr(),
                FORMAT_INT64,
                (&raw mut number).cast(),
            )
        };
        (code >= 0).then_some(number)
    }

    pub fn get_double(&self, name: &str) -> Option<f64> {
        let name = c(name);
        let mut number: f64 = 0.0;
        // SAFETY: DOUBLE writes a double.
        let code = unsafe {
            (self.api.get_property)(
                self.ctx,
                name.as_ptr(),
                FORMAT_DOUBLE,
                (&raw mut number).cast(),
            )
        };
        (code >= 0 && number.is_finite()).then_some(number)
    }

    pub fn get_string(&self, name: &str) -> Option<String> {
        let name = c(name);
        let mut pointer: *mut c_char = std::ptr::null_mut();
        // SAFETY: STRING writes a char pointer mpv allocated; it is freed with mpv_free.
        let code = unsafe {
            (self.api.get_property)(
                self.ctx,
                name.as_ptr(),
                FORMAT_STRING,
                (&raw mut pointer).cast(),
            )
        };
        if code < 0 || pointer.is_null() {
            return None;
        }
        // SAFETY: a valid C string from mpv, freed right after copying.
        let value = unsafe { text(pointer) };
        unsafe { (self.api.free)(pointer.cast()) };
        value
    }

    pub fn observe(&self) -> Result<(), c_int> {
        for (index, (name, format)) in OBSERVED.iter().enumerate() {
            let name = c(name);
            // SAFETY: valid handle and C string; mpv copies the name.
            Self::check(unsafe {
                (self.api.observe_property)(self.ctx, index as u64, name.as_ptr(), *format)
            })?;
        }
        Ok(())
    }

    pub fn request_logs(&self, level: &str) -> Result<(), c_int> {
        let level = c(level);
        // SAFETY: valid handle and C string.
        Self::check(unsafe { (self.api.request_log_messages)(self.ctx, level.as_ptr()) })
    }

    /// Wakes a `wait` that is waiting.
    pub fn wakeup(&self) {
        // SAFETY: valid handle; safe from any thread.
        unsafe { (self.api.wakeup)(self.ctx) }
    }

    /// The next event, waiting at most `timeout` seconds. Everything is copied out before
    /// returning, because mpv reuses the event's memory on the next call.
    pub fn wait(&self, timeout: f64) -> Waited {
        // SAFETY: valid handle; only the event thread calls this.
        let event = unsafe { (self.api.wait_event)(self.ctx, timeout) };
        if event.is_null() {
            return Waited::Nothing;
        }
        // SAFETY: mpv returns a valid event; its data matches its id (client.h).
        unsafe {
            let event = &*event;
            match event.event_id {
                EVENT_NONE => Waited::Nothing,
                EVENT_SHUTDOWN => Waited::Shutdown,
                EVENT_QUEUE_OVERFLOW => Waited::Overflow,
                EVENT_FILE_LOADED => Waited::FileLoaded,
                EVENT_START_FILE if !event.data.is_null() => {
                    let start = &*event.data.cast::<RawStartFile>();
                    Waited::Event(Event::StartFile {
                        entry: start.playlist_entry_id,
                    })
                }
                EVENT_END_FILE if !event.data.is_null() => {
                    let end = &*event.data.cast::<RawEndFile>();
                    let reason = match end.reason {
                        0 => EndReason::Eof,
                        2 | 3 => EndReason::Stop,
                        4 => EndReason::Error,
                        _ => EndReason::Other,
                    };
                    Waited::Event(Event::EndFile {
                        entry: end.playlist_entry_id,
                        reason,
                        error: end.error,
                    })
                }
                EVENT_LOG_MESSAGE if !event.data.is_null() => {
                    let log = &*event.data.cast::<RawLogMessage>();
                    let prefix = text(log.prefix).unwrap_or_default();
                    let line = text(log.text).unwrap_or_default();
                    Waited::Log(
                        text(log.level).unwrap_or_default(),
                        format!("[{prefix}] {}", line.trim_end()),
                    )
                }
                EVENT_PROPERTY_CHANGE if !event.data.is_null() => {
                    let property = &*event.data.cast::<RawProperty>();
                    property_event(event.reply_userdata, property)
                }
                _ => Waited::Nothing,
            }
        }
    }
}

/// # Safety
/// `property.data` matches `property.format`, as mpv guarantees.
unsafe fn property_event(id: u64, property: &RawProperty) -> Waited {
    let double = || {
        (property.format == FORMAT_DOUBLE && !property.data.is_null())
            .then(|| unsafe { *property.data.cast::<f64>() })
    };
    let flag = || {
        (property.format == FORMAT_FLAG && !property.data.is_null())
            .then(|| unsafe { *property.data.cast::<c_int>() } != 0)
    };
    // STRING data is a pointer to a C string (mpv frees it).
    let string = || {
        (property.format == FORMAT_STRING && !property.data.is_null())
            .then(|| unsafe { text(*property.data.cast::<*const c_char>()) })
            .flatten()
    };
    let event = match id {
        0 => Event::Position(double()),
        1 => Event::Duration(double()),
        2 => match flag() {
            Some(paused) => Event::Pause(paused),
            None => return Waited::Nothing,
        },
        3 => Event::EndReached(flag().unwrap_or(false)),
        4 => match double() {
            Some(speed) => Event::Speed(speed),
            None => return Waited::Nothing,
        },
        5 => match double() {
            Some(volume) => Event::Volume(volume),
            None => return Waited::Nothing,
        },
        6 => Event::VideoShown(flag().unwrap_or(false)),
        7 => Event::HardwareDecoder(string().filter(|name| !name.is_empty() && name != "no")),
        _ => return Waited::Nothing,
    };
    Waited::Event(event)
}

/// mpv's software renderer: mpv draws each video frame into memory the app gives it
/// (iOS shows it; tests look at it). Free it before the handle it came from: it must
/// go before `mpv_terminate_destroy`.
pub(super) struct Frames {
    api: &'static Api,
    ctx: *mut c_void,
    /// Called from mpv's threads when a new frame is ready; boxed so its address stays.
    on_frame: *mut Box<dyn Fn() + Send + Sync>,
}

unsafe impl Send for Frames {}
unsafe impl Sync for Frames {}

unsafe extern "C" fn frame_ready(data: *mut c_void) {
    // SAFETY: `data` is the Frames' boxed callback, freed only after the render context.
    let on_frame = unsafe { &*data.cast::<Box<dyn Fn() + Send + Sync>>() };
    // A panic must not cross into mpv's C code.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(on_frame));
}

impl Handle {
    /// The software renderer on this handle (vo=libmpv must be set before initialize).
    pub fn frames(&self, on_frame: Box<dyn Fn() + Send + Sync>) -> Result<Frames, c_int> {
        let kind = c("sw");
        let mut params = [
            RenderParam {
                kind: RENDER_PARAM_API_TYPE,
                data: kind.as_ptr().cast_mut().cast(),
            },
            RenderParam {
                kind: RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let mut ctx: *mut c_void = std::ptr::null_mut();
        // SAFETY: valid handle; the parameter list ends with INVALID; `kind` outlives it.
        Self::check(unsafe {
            (self.api.render_create)(&raw mut ctx, self.ctx, params.as_mut_ptr())
        })?;
        let on_frame = Box::into_raw(Box::new(on_frame));
        // SAFETY: a valid render context and a callback that lives as long as it.
        unsafe { (self.api.render_set_callback)(ctx, Some(frame_ready), on_frame.cast()) };
        Ok(Frames {
            api: self.api,
            ctx,
            on_frame,
        })
    }
}

impl Frames {
    /// Whether a new frame is there to draw (call after the callback).
    pub fn has_new_frame(&self) -> bool {
        // SAFETY: a valid render context.
        unsafe { (self.api.render_update)(self.ctx) & RENDER_UPDATE_FRAME != 0 }
    }

    /// Draws the current frame, scaled to fit with black around, into `pixels`: `width`
    /// by `height` pixels, 4 bytes each (blue, green, red, unused: "bgr0"), `stride` bytes
    /// a line.
    ///
    /// # Safety
    /// `pixels` points to at least `stride * height` writable bytes for the whole call.
    pub unsafe fn draw(
        &self,
        width: i32,
        height: i32,
        stride: usize,
        pixels: *mut u8,
    ) -> Result<(), c_int> {
        let mut size = [width as c_int, height as c_int];
        let format = c("bgr0");
        let mut stride = stride;
        let mut params = [
            RenderParam {
                kind: RENDER_PARAM_SW_SIZE,
                data: size.as_mut_ptr().cast(),
            },
            RenderParam {
                kind: RENDER_PARAM_SW_FORMAT,
                data: format.as_ptr().cast_mut().cast(),
            },
            RenderParam {
                kind: RENDER_PARAM_SW_STRIDE,
                data: (&raw mut stride).cast(),
            },
            RenderParam {
                kind: RENDER_PARAM_SW_POINTER,
                data: pixels.cast(),
            },
            RenderParam {
                kind: RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        // SAFETY: valid context; every parameter points to live data of its type.
        Handle::check(unsafe { (self.api.render_render)(self.ctx, params.as_mut_ptr()) })
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        // SAFETY: the context is valid until here; no callback comes after it is freed,
        // so the callback can go after it.
        unsafe {
            (self.api.render_free)(self.ctx);
            drop(Box::from_raw(self.on_frame));
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the last user of the handle; mpv stops playback and frees it.
        unsafe { (self.api.terminate_destroy)(self.ctx) }
        if self.closing.load(Ordering::SeqCst) {
            let mut closing = CLOSING
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *closing = closing.saturating_sub(1);
            CLOSED.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_library_is_an_error_not_a_crash() {
        let Err(error) = load(vec![
            "/nowhere/libmpv.so".to_owned(),
            "libnot-mpv.so".to_owned(),
        ]) else {
            panic!("there is no such library");
        };
        assert!(error.starts_with("libmpv is not there"), "{error}");
        assert!(
            error.contains("/nowhere/libmpv.so") && error.contains("libnot-mpv.so"),
            "{error}"
        );
    }

    #[test]
    fn strings_for_mpv_never_carry_a_nul() {
        assert_eq!(c("pause").as_bytes(), b"pause");
        assert_eq!(c("a\0b").as_bytes(), b"a");
    }
}
