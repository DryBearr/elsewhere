//! Android: mpv draws video into the app's Surface, which only Java has. Plain JNI, with
//! no crate for it (two functions of JNI's table are used), and only for what JNI must
//! do; attaching goes through the core's API like everything else (CoreQueue's
//! attach_surface), because this library can be in the app twice: once loaded by JNA (the
//! API), once by System.loadLibrary (these functions), each with its own statics. libmpv is
//! there once, so what is handed to it here is seen by every player:
//! - JNI_OnLoad keeps the Java VM; it goes to libmpv's FFmpeg, which mpv's video output
//!   needs to reach the Surface;
//! - VideoSurface.window turns a Surface into a JNI global reference (mpv's `wid`), and
//!   VideoSurface.release lets it go once mpv is done with it (playback/VideoSurface.kt).

use std::ffi::{c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use crate::logs;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

const RTLD_NOW: c_int = 2;
const JNI_VERSION_1_6: c_int = 0x0001_0006;
/// Indexes into JNI's function table (jni.h).
const NEW_GLOBAL_REF: usize = 21;
const DELETE_GLOBAL_REF: usize = 22;

/// A JNIEnv*: a pointer to the pointer to JNI's function table.
type Env = *mut *const *const c_void;
type Object = *mut c_void;

static VM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static HANDED: AtomicBool = AtomicBool::new(false);

/// Called by Java when System.loadLibrary loads this library.
#[unsafe(no_mangle)]
pub extern "system" fn JNI_OnLoad(vm: *mut c_void, _reserved: *mut c_void) -> c_int {
    VM.store(vm, Ordering::SeqCst);
    JNI_VERSION_1_6
}

/// libmpv was loaded by a player (`library` is its handle): it gets the VM if this copy
/// of the library has it.
pub(super) fn hand_vm(library: *mut c_void) {
    hand(library);
}

/// Gives the Java VM to libmpv's FFmpeg (once per copy of this library; FFmpeg keeps one).
fn hand(library: *mut c_void) {
    let vm = VM.load(Ordering::SeqCst);
    if vm.is_null() || library.is_null() || HANDED.swap(true, Ordering::SeqCst) {
        return;
    }
    // Exported by our libmpv build next to mpv_* (scripts/android_mpv.sh).
    // SAFETY: a valid library handle and C string.
    let set = unsafe { dlsym(library, c"av_jni_set_java_vm".as_ptr()) };
    if set.is_null() {
        logs::error(
            "player",
            "libmpv has no av_jni_set_java_vm: no video on Android",
        )
        .write();
        HANDED.store(false, Ordering::SeqCst);
        return;
    }
    // SAFETY: FFmpeg's int av_jni_set_java_vm(void *vm, void *log_ctx).
    let set: unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int =
        unsafe { std::mem::transmute(set) };
    // SAFETY: the VM Java gave JNI_OnLoad.
    unsafe { set(vm, std::ptr::null_mut()) };
}

/// The JNI function at `index` of `env`'s table.
///
/// # Safety
/// `env` is the JNIEnv of the calling thread.
unsafe fn jni(env: Env, index: usize) -> *const c_void {
    unsafe { *(*env).add(index) }
}

/// VideoSurface.window(surface): a JNI global reference to `surface` for mpv (0 if none
/// could be made). The app passes it to CoreQueue.attach_surface and releases it after.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_drybearr_elsewhere_playback_VideoSurface_window(
    env: Env,
    _class: Object,
    surface: Object,
) -> i64 {
    catch_unwind(AssertUnwindSafe(|| {
        if surface.is_null() {
            return 0;
        }
        // libmpv is loaded already (a player exists): this finds it.
        // SAFETY: dlopen with a C string; a null result is handled in `hand`.
        hand(unsafe { dlopen(c"libmpv.so".as_ptr(), RTLD_NOW) });
        // SAFETY: NewGlobalRef(JNIEnv*, jobject) -> jobject.
        let new_global: extern "system" fn(Env, Object) -> Object =
            unsafe { std::mem::transmute(jni(env, NEW_GLOBAL_REF)) };
        new_global(env, surface) as i64
    }))
    .unwrap_or(0)
}

/// VideoSurface.release(window): lets go of a reference from `window`, once mpv no longer
/// draws into it (after CoreQueue.detach_video returned).
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_drybearr_elsewhere_playback_VideoSurface_release(
    env: Env,
    _class: Object,
    window: i64,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if window == 0 {
            return;
        }
        // SAFETY: DeleteGlobalRef(JNIEnv*, jobject) on a reference `window` made.
        let delete: extern "system" fn(Env, Object) =
            unsafe { std::mem::transmute(jni(env, DELETE_GLOBAL_REF)) };
        delete(env, window as Object);
    }));
}
