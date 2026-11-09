//! Tests for showing video (src/player/video.rs) with the real libmpv: mpv's software
//! renderer draws frames into memory here, the way the iOS app draws them on screen. Own
//! test program: frames output is set for every player of the process. No sound output.
//! The clips in tests/media are 96x64 and 2 s long.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once};
use std::thread;
use std::time::{Duration, Instant};

use elsewhere::player::{self, Player, PlayerState, Status};
use elsewhere::problem::Kind;

fn media_dir() -> PathBuf {
    std::env::var_os("PLAYER_TEST_MEDIA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/media"))
}

fn media(name: &str) -> String {
    media_dir().join(name).to_str().unwrap().to_owned()
}

fn new_player() -> Player {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        player::set_null_output(true);
        player::set_frames_output(true);
        player::allow_folders(&[media_dir()]);
    });
    Player::new().expect("libmpv loads (set ELSEWHERE_LIBMPV if it is not installed)")
}

fn wait(player: &Player, what: &str, check: impl Fn(&PlayerState) -> bool) -> PlayerState {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let state = player.state();
        if check(&state) {
            return state;
        }
        assert!(Instant::now() < deadline, "never {what}: {state:?}");
        thread::sleep(Duration::from_millis(5));
    }
}

fn until(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(25);
    while !check() {
        assert!(Instant::now() < deadline, "never {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

/// A frame buffer: width x height pixels of 4 bytes.
struct Frame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Frame {
    fn new(width: u32, height: u32) -> Frame {
        Frame {
            width,
            height,
            pixels: vec![0; (width * height * 4) as usize],
        }
    }

    fn draw(&mut self, video: &player::Video) -> Result<bool, elsewhere::problem::Problem> {
        // SAFETY: the buffer holds stride * height bytes and lives through the call.
        unsafe {
            video.draw(
                self.width,
                self.height,
                self.width as usize * 4,
                self.pixels.as_mut_ptr(),
            )
        }
    }

    /// Whether some pixel is not black.
    fn has_picture(&self) -> bool {
        self.pixels
            .chunks(4)
            .any(|pixel| pixel[0] > 16 || pixel[1] > 16 || pixel[2] > 16)
    }
}

#[test]
fn a_video_draws_its_frames_once_attached() {
    let player = new_player();
    let video = player.video();
    player.open(&media("clip.mp4"), 0, false).unwrap();
    let paused = wait(&player, "paused", |state| state.status == Status::Paused);
    assert_eq!(paused.video_size, Some((96, 64)), "{paused:?}");
    let mut frame = Frame::new(192, 128);
    // Not attached: nothing is drawn.
    assert!(!frame.draw(&video).unwrap());
    assert!(!frame.has_picture());
    let frames = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&frames);
    video
        .attach_frames(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
    until("a first frame", || frames.load(Ordering::SeqCst) > 0);
    assert!(frame.draw(&video).unwrap());
    assert!(frame.has_picture(), "the paused clip's first frame");
    // Playing on, more frames come.
    let before = frames.load(Ordering::SeqCst);
    player.play().unwrap();
    until("more frames", || frames.load(Ordering::SeqCst) > before + 5);
    assert_eq!(
        wait(&player, "the end", |state| state.status == Status::Ended).status,
        Status::Ended
    );
}

#[test]
fn detached_video_draws_nothing_and_the_sound_plays_on() {
    let player = new_player();
    let video = player.video();
    let frames = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&frames);
    video
        .attach_frames(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
    player.open(&media("clip.mp4"), 0, true).unwrap();
    until("frames", || frames.load(Ordering::SeqCst) > 2);
    video.detach().unwrap();
    // Detaching twice is fine.
    video.detach().unwrap();
    let mut frame = Frame::new(96, 64);
    assert!(!frame.draw(&video).unwrap(), "detached: nothing drawn");
    let after = frames.load(Ordering::SeqCst);
    let end = wait(&player, "the end", |state| state.status == Status::Ended);
    assert!(
        end.position_ms >= 1500,
        "the sound played on to the end: {end:?}"
    );
    assert!(
        frames.load(Ordering::SeqCst) <= after + 1,
        "no frames after detaching (one may have been on its way)"
    );
}

#[test]
fn wrong_frames_are_refused() {
    let player = new_player();
    let video = player.video();
    video.attach_frames(|| {}).unwrap();
    let mut pixels = vec![0u8; 64 * 64 * 4];
    let pointer = pixels.as_mut_ptr();
    for (width, height, stride) in [
        (0, 64, 256),
        (64, 0, 256),
        (64, 64, 100),
        (64, 64, 258),
        (9000, 1, 36_000),
    ] {
        // SAFETY: refused before anything is written.
        let result = unsafe { video.draw(width, height, stride, pointer) };
        assert_eq!(
            result.unwrap_err().kind,
            Kind::Input,
            "{width}x{height} stride {stride}"
        );
    }
    // SAFETY: a null pointer is refused.
    let null = unsafe { video.draw(64, 64, 256, std::ptr::null_mut()) };
    assert_eq!(null.unwrap_err().kind, Kind::Input);
    // A surface is for Android's own output, not for frames.
    assert_eq!(video.attach_surface(1).unwrap_err().kind, Kind::Bug);
}

#[test]
fn a_closed_player_draws_nothing() {
    let player = new_player();
    let video = player.video();
    video.attach_frames(|| {}).unwrap();
    drop(player);
    let mut frame = Frame::new(96, 64);
    // The player goes away on its own thread: soon the handle says so.
    until("the player gone", || frame.draw(&video).is_err());
    assert_eq!(frame.draw(&video).unwrap_err().kind, Kind::Input);
    assert_eq!(video.attach_frames(|| {}).unwrap_err().kind, Kind::Input);
    assert_eq!(video.detach().unwrap_err().kind, Kind::Input);
}

#[test]
fn songs_have_no_picture_size() {
    let player = new_player();
    let video = player.video();
    video.attach_frames(|| {}).unwrap();
    player.open(&media("song.m4a"), 0, false).unwrap();
    let paused = wait(&player, "paused", |state| state.status == Status::Paused);
    assert_eq!((paused.has_video, paused.video_size), (false, None));
    // Drawing a song draws black, and works.
    let mut frame = Frame::new(32, 32);
    assert!(frame.draw(&video).unwrap());
    assert!(!frame.has_picture());
    // A new file forgets the old size.
    player.open(&media("clip.mp4"), 0, false).unwrap();
    wait(&player, "the clip's size", |state| {
        state.video_size == Some((96, 64))
    });
    player.open(&media("song.m4a"), 0, false).unwrap();
    wait(&player, "no size", |state| {
        state.status == Status::Paused && state.video_size.is_none()
    });
}

#[test]
fn attaching_and_detaching_again_and_again_while_drawing() {
    let player = new_player();
    let video = player.video();
    player.set_speed(0.5).unwrap();
    player.open(&media("clip.mp4"), 0, true).unwrap();
    // One thread draws all along, like a screen does.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let drawer = {
        let (video, stop) = (video.clone(), Arc::clone(&stop));
        thread::spawn(move || {
            let mut frame = Frame::new(48, 32);
            let mut drawn = 0;
            while !stop.load(Ordering::SeqCst) {
                if frame.draw(&video).unwrap() {
                    drawn += 1;
                }
                thread::sleep(Duration::from_millis(1));
            }
            drawn
        })
    };
    for round in 0..60 {
        video.attach_frames(|| {}).unwrap();
        thread::sleep(Duration::from_millis(round % 7));
        video.detach().unwrap();
    }
    video.attach_frames(|| {}).unwrap();
    let end = wait(&player, "the end", |state| {
        matches!(state.status, Status::Ended | Status::Failed)
    });
    stop.store(true, Ordering::SeqCst);
    assert!(drawer.join().unwrap() > 0, "some frames were drawn");
    assert_eq!(end.status, Status::Ended, "{end:?}");
    assert!(end.problem.is_none());
}
