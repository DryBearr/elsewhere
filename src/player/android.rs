//! Android: mpv draws video into the app's Surface, which only Java has. Plain JNI, with
//! no crate for it (two functions of JNI's table are used):
//! - the app also loads this library with System.loadLibrary("elsewhere"), so JNI_OnLoad
//!   keeps the Java VM, which mpv's video output needs to reach the Surface;
//! - playback/VideoSurface.kt hands Surfaces over through the Java_... functions below,
//!   naming the player by its number (`Video::target`).

use std::ffi::{c_char, c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use super::video::{Video, by_target};
use crate::logs;

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

const JNI_VERSION_1_6: c_int = 0x0001_0006;
/// Indexes into JNI's function table (jni.h).
const NEW_GLOBAL_REF: usize = 21;
const DELETE_GLOBAL_REF: usize = 22;

/// A JNIEnv*: a pointer to the pointer to JNI's function table.
type Env = *mut *const *const c_void;
type Object = *mut c_void;

static VM: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static LIBMPV: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static HANDED: AtomicBool = AtomicBool::new(false);

/// The Surfaces handed to players, as JNI global references: (player number, reference).
static SURFACES: Mutex<Vec<(i64, usize)>> = Mutex::new(Vec::new());

/// Called by Java when System.loadLibrary loads this library.
#[unsafe(no_mangle)]
pub extern "system" fn JNI_OnLoad(vm: *mut c_void, _reserved: *mut c_void) -> c_int {
    VM.store(vm, Ordering::SeqCst);
    hand();
    JNI_VERSION_1_6
}

/// libmpv was loaded (`library` is its dlopen handle).
pub(super) fn hand_vm(library: *mut c_void) {
    LIBMPV.store(library, Ordering::SeqCst);
    hand();
}

/// Gives the Java VM to libmpv's FFmpeg once both are there.
fn hand() {
    let (vm, library) = (VM.load(Ordering::SeqCst), LIBMPV.load(Ordering::SeqCst));
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

/// # Safety
/// `env` is the calling thread's JNIEnv and `reference` a global reference from it.
unsafe fn delete_global(env: Env, reference: usize) {
    // SAFETY: DeleteGlobalRef(JNIEnv*, jobject).
    let delete: extern "system" fn(Env, Object) =
        unsafe { std::mem::transmute(jni(env, DELETE_GLOBAL_REF)) };
    delete(env, reference as Object);
}

fn lock() -> std::sync::MutexGuard<'static, Vec<(i64, usize)>> {
    SURFACES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Detaches the player's video (if it lives) and lets its Surfaces go.
///
/// # Safety
/// `env` is the calling thread's JNIEnv.
unsafe fn let_go(env: Env, target: i64, video: Option<&Video>) {
    if let Some(video) = video
        && let Err(problem) = video.detach()
    {
        logs::warn("player", "Detaching a Surface failed")
            .detail(problem.detail)
            .write();
    }
    let mut surfaces = lock();
    surfaces.retain(|&(owner, reference)| {
        if owner != target {
            return true;
        }
        // SAFETY: a global reference this file made; mpv no longer uses it.
        unsafe { delete_global(env, reference) };
        false
    });
}

/// VideoSurface.attach(target, surface): video on, drawn into `surface`. True if it worked.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_drybearr_elsewhere_playback_VideoSurface_attach(
    env: Env,
    _class: Object,
    target: i64,
    surface: Object,
) -> u8 {
    catch_unwind(AssertUnwindSafe(|| {
        hand();
        let Some(video) = by_target(target) else {
            return 0;
        };
        if surface.is_null() {
            return 0;
        }
        // An earlier Surface goes first (mpv takes one at a time).
        // SAFETY: called by Java on its thread with its JNIEnv.
        unsafe { let_go(env, target, Some(&video)) };
        // SAFETY: NewGlobalRef(JNIEnv*, jobject) -> jobject.
        let new_global: extern "system" fn(Env, Object) -> Object =
            unsafe { std::mem::transmute(jni(env, NEW_GLOBAL_REF)) };
        let global = new_global(env, surface);
        if global.is_null() {
            return 0;
        }
        match video.attach_surface(global as i64) {
            Ok(()) => {
                lock().push((target, global as usize));
                1
            }
            Err(problem) => {
                logs::warn("player", "Attaching a Surface failed")
                    .detail(problem.detail)
                    .write();
                // SAFETY: the reference just made, which mpv did not keep.
                let _ = video.detach();
                unsafe { delete_global(env, global as usize) };
                0
            }
        }
    }))
    .unwrap_or(0)
}

/// VideoSurface.detach(target): video off; when it returns, the Surface is free.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_drybearr_elsewhere_playback_VideoSurface_detach(
    env: Env,
    _class: Object,
    target: i64,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let video = by_target(target);
        // SAFETY: called by Java on its thread with its JNIEnv.
        unsafe { let_go(env, target, video.as_ref()) };
    }));
}

/// VideoSurface.resize(target, width, height): the Surface's new size.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_drybearr_elsewhere_playback_VideoSurface_resize(
    _env: Env,
    _class: Object,
    target: i64,
    width: c_int,
    height: c_int,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let (Some(video), Ok(width), Ok(height)) = (
            by_target(target),
            u32::try_from(width),
            u32::try_from(height),
        ) && let Err(problem) = video.surface_size(width, height)
        {
            logs::warn("player", "Resizing a Surface failed")
                .detail(problem.detail)
                .write();
        }
    }));
}
