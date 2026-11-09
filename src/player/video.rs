//! Showing video. A player starts with video off (vid=no): it plays only the sound, also
//! for a video file, until a screen that shows the picture attaches. While attached the
//! picture plays too; detaching turns it off again and the sound plays on (that is how a
//! video goes on in the background, or behind the lock screen).
//!
//! Two ways to attach, one per platform's drawing:
//! - frames (iOS, and the tests): mpv draws each frame into memory the app hands it
//!   (`draw`), told by a callback when a new one is ready. Needs `set_frames_output(true)`
//!   before the player is made (the core's start does it on iOS).
//! - a surface (Android): mpv draws into the app's Surface itself (`attach_surface`, with
//!   a JNI global reference to it from android.rs).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use super::{Inner, mpv};
use crate::problem::{Kind, Problem};

/// vo=libmpv: the app draws frames itself (iOS has no other video output).
static FRAMES_OUTPUT: AtomicBool = AtomicBool::new(false);

/// Largest frame that can be drawn (8K).
const MAX_SIDE: u32 = 8192;

/// Players made after this draw their video as frames for the app (`Video::attach_frames`).
pub fn set_frames_output(on: bool) {
    FRAMES_OUTPUT.store(on, Ordering::SeqCst);
}

pub(super) fn frames_output() -> bool {
    FRAMES_OUTPUT.load(Ordering::SeqCst)
}

type OnFrame = Arc<dyn Fn() + Send + Sync>;

/// A player's video, kept in its Inner (before the mpv handle, so it is freed first).
#[derive(Default)]
pub(super) struct Slot {
    frames: Option<mpv::Frames>,
    /// Who hears about new frames now (none while detached).
    on_frame: Arc<Mutex<Option<OnFrame>>>,
    attached: Attached,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Attached {
    #[default]
    Nothing,
    Frames,
    Surface,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn gone() -> Problem {
    Problem {
        retry: false,
        ..Problem::new(
            Kind::Input,
            "The player is closed.",
            "the player was dropped",
        )
    }
}

fn mpv_problem(what: &str, code: i32, inner: &Inner) -> Problem {
    Problem::bug(format!("{what}: {}", inner.mpv.error_text(code)))
}

/// A handle on one player's video; it does not keep the player alive.
#[derive(Clone)]
pub struct Video {
    pub(super) inner: Weak<Inner>,
}

impl Video {
    fn inner(&self) -> Result<Arc<Inner>, Problem> {
        self.inner.upgrade().ok_or_else(gone)
    }

    /// Video on, drawn as frames: `on_frame` is called (on one of mpv's threads; hand it
    /// over and return) whenever a new frame is ready for `draw`. Replaces an earlier one.
    pub fn attach_frames(
        &self,
        on_frame: impl Fn() + Send + Sync + 'static,
    ) -> Result<(), Problem> {
        let inner = self.inner()?;
        if !frames_output() {
            return Err(Problem::bug(
                "this player draws video into a surface, not as frames (set_frames_output)",
            ));
        }
        let mut slot = lock(&inner.video);
        if slot.frames.is_none() {
            let listener = Arc::clone(&slot.on_frame);
            let frames = inner
                .mpv
                .frames(Box::new(move || {
                    let on_frame = lock(&listener).clone();
                    if let Some(on_frame) = on_frame {
                        on_frame();
                    }
                }))
                .map_err(|code| mpv_problem("making the video renderer", code, &inner))?;
            slot.frames = Some(frames);
        }
        *lock(&slot.on_frame) = Some(Arc::new(on_frame));
        slot.attached = Attached::Frames;
        inner
            .mpv
            .set_string("vid", "auto")
            .map_err(|code| mpv_problem("turning video on", code, &inner))
    }

    /// Draws the current frame (scaled to fit, black around it) into `pixels`: `width`
    /// by `height` pixels of 4 bytes (blue, green, red, unused), `stride` bytes a line.
    /// False when nothing is attached as frames (nothing is drawn then).
    ///
    /// # Safety
    /// `pixels` points to at least `stride * height` writable bytes for the whole call.
    pub unsafe fn draw(
        &self,
        width: u32,
        height: u32,
        stride: usize,
        pixels: *mut u8,
    ) -> Result<bool, Problem> {
        let inner = self.inner()?;
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(Problem::input(format!(
                "A frame of {width}x{height} cannot be drawn."
            )));
        }
        if stride < width as usize * 4 || !stride.is_multiple_of(4) || pixels.is_null() {
            return Err(Problem::input("That is not a frame to draw into."));
        }
        let slot = lock(&inner.video);
        let (Attached::Frames, Some(frames)) = (slot.attached, slot.frames.as_ref()) else {
            return Ok(false);
        };
        let _ = frames.has_new_frame();
        // SAFETY: the caller's promise about `pixels`; sizes checked above.
        unsafe { frames.draw(width as i32, height as i32, stride, pixels) }
            .map_err(|code| mpv_problem("drawing a frame", code, &inner))?;
        Ok(true)
    }

    /// Video on, drawn by mpv into an Android Surface: `window` is a JNI global
    /// reference to it, valid until `detach` returns.
    pub fn attach_surface(&self, window: i64) -> Result<(), Problem> {
        let inner = self.inner()?;
        if frames_output() || !cfg!(target_os = "android") {
            return Err(Problem::bug(
                "only Android players draw video into a surface",
            ));
        }
        if window == 0 {
            return Err(Problem::input("That is not a surface."));
        }
        let mut slot = lock(&inner.video);
        let set = |name: &str, value: &str| {
            inner
                .mpv
                .set_option(name, value)
                .map_err(|code| mpv_problem(&format!("option {name}"), code, &inner))
        };
        set("wid", &window.to_string())?;
        inner
            .mpv
            .set_string("vo", "gpu")
            .map_err(|code| mpv_problem("the video output", code, &inner))?;
        slot.attached = Attached::Surface;
        let on = inner
            .mpv
            .set_string("vid", "auto")
            .map_err(|code| mpv_problem("turning video on", code, &inner));
        crate::logs::info(
            "player",
            format!("{}: video on, into a surface", inner.name),
        )
        .write();
        on
    }

    /// What mpv says about its video now, as one line (for the apps' tests).
    pub fn describe(&self) -> String {
        let Ok(inner) = self.inner() else {
            return "player closed".to_owned();
        };
        let attached = lock(&inner.video).attached;
        let mpv = &inner.mpv;
        let text = |name: &str| mpv.get_string(name).unwrap_or_else(|| "-".to_owned());
        format!(
            "{} at {:p}: {attached:?} vo={} current-vo={} vid={} wid={} vo-configured={} video={}x{}",
            inner.name,
            Arc::as_ptr(&inner),
            text("vo"),
            text("current-vo"),
            text("vid"),
            text("wid"),
            text("vo-configured"),
            text("video-params/w"),
            text("video-params/h"),
        )
    }

    /// The Android Surface's size changed (rotation, split screen).
    pub fn surface_size(&self, width: u32, height: u32) -> Result<(), Problem> {
        let inner = self.inner()?;
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(Problem::input(format!(
                "A surface of {width}x{height} is not one."
            )));
        }
        inner
            .mpv
            .set_option("android-surface-size", &format!("{width}x{height}"))
            .map_err(|code| mpv_problem("the surface size", code, &inner))
    }

    /// Video off; the sound plays on. When it returns, mpv no longer touches the Surface
    /// or calls back about frames. Fine to call when nothing is attached.
    pub fn detach(&self) -> Result<(), Problem> {
        let inner = self.inner()?;
        crate::logs::info("player", format!("{}: video off", inner.name)).write();
        let mut slot = lock(&inner.video);
        *lock(&slot.on_frame) = None;
        let was = std::mem::take(&mut slot.attached);
        let off = inner
            .mpv
            .set_string("vid", "no")
            .map_err(|code| mpv_problem("turning video off", code, &inner));
        if was == Attached::Surface {
            // The output lets go of the Surface before the app's reference to it goes.
            inner
                .mpv
                .set_string("vo", "null")
                .map_err(|code| mpv_problem("the video output", code, &inner))?;
            let _ = inner.mpv.set_option("wid", "0");
        }
        off
    }
}
