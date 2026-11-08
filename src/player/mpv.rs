//! libmpv, loaded when the first player is made (dlopen), so the core builds and starts
//! without it, and only apps that play something pay for it. The functions used are a
//! small, stable part of mpv's C API (client.h, API version 2), declared here by hand.
//!
//! Which library: the file named by ELSEWHERE_LIBMPV (desktop development), else the
//! platform's usual name (libmpv.so on Android and Linux, libmpv.2.dylib on macOS).

use std::ffi::{CStr, CString, c_char, c_int, c_ulong, c_void};
use std::sync::OnceLock;

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
}

/// Only function pointers into a library that stays loaded.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

fn library_names() -> Vec<String> {
    let mut names: Vec<String> = std::env::var("ELSEWHERE_LIBMPV").into_iter().collect();
    let platform: &[&str] = if cfg!(target_os = "macos") {
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
    };
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
pub(super) const OBSERVED: [(&str, c_int); 6] = [
    ("time-pos", FORMAT_DOUBLE),
    ("duration", FORMAT_DOUBLE),
    ("pause", FORMAT_FLAG),
    ("eof-reached", FORMAT_FLAG),
    ("speed", FORMAT_DOUBLE),
    ("volume", FORMAT_DOUBLE),
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
        Ok(Handle { api, ctx })
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
        _ => return Waited::Nothing,
    };
    Waited::Event(event)
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the last user of the handle; mpv stops playback and frees it.
        unsafe { (self.api.terminate_destroy)(self.ctx) }
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
