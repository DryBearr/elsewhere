import AVFoundation
import Combine
import MediaPlayer
import UIKit

/// The app's playlist player: one CoreQueue for the whole app. It plays on in the background
/// (UIBackgroundModes audio in project.yml) and shows on the lock screen and in Control
/// Center, where its buttons (and those of headphones, cars and Bluetooth) control it.
/// Call it on the main thread; the calls into the core run one after the other on its own
/// queue (in the order they were made), and states come back on the main thread.
///
/// It also does what iOS expects of a media app:
/// - an interruption (a call, an alarm, Siri) pauses it, and it plays on after it when iOS
///   says so; a pause by the person during it stays a pause;
/// - unplugged headphones (or Bluetooth gone) pause it;
/// - after iOS restarts its media services, the audio is set up again.
///
/// Plugged in by one line in ElsewhereApp (`Playback.shared.start()`) and the background
/// mode in project.yml.
final class Playback: ObservableObject {
    static let shared = Playback()

    /// The newest state; nil until something is played.
    @Published private(set) var state: QueueState?
    /// Why the last command did nothing (refused, no sound during a call); nil after one works.
    @Published private(set) var message: String?
    /// The saved playlist the queue was made from, if it was.
    @Published private(set) var source: Int64?
    /// The queue's entries, while a screen wants them (`wantEntries`); else possibly old.
    @Published private(set) var entries: QueueEntries?
    /// Whether the player screen shows (the player bar opens it).
    @Published var playerOpen = false
    private var entryScreens = 0
    private var fetching: Int64?

    /// Commands that start playback and have not answered yet.
    private var starting = 0
    /// Paused by an interruption (not by the person): plays on when it ends.
    private var resumeAfterInterruption = false
    private var sessionActive = false
    private var started = false

    /// One queue, so commands reach the core in the order they were made.
    private let commands = DispatchQueue(label: "elsewhere-playback")
    /// Touched on `commands` only.
    private var queue: CoreQueue?
    /// The same queue for drawing video frames, which does not go through `commands`.
    private let drawLock = NSLock()
    private var drawing: CoreQueue?
    /// The video views showing, newest last (main thread): the newest gets the frames.
    private var videoViews: [VideoFrames] = []

    private init() {}

    /// Whether playback goes on, is about to, or waits for an interruption to end.
    var keepsGoing: Bool {
        state?.wantsPlay == true || starting > 0 || resumeAfterInterruption
    }

    /// Listens to the system: audio session notifications and the remote commands.
    func start() {
        guard !started else { return }
        started = true
        let center = NotificationCenter.default
        let session = AVAudioSession.sharedInstance()
        center.addObserver(forName: AVAudioSession.interruptionNotification, object: session, queue: .main) { [weak self] note in
            self?.interrupted(note)
        }
        center.addObserver(forName: AVAudioSession.routeChangeNotification, object: session, queue: .main) { [weak self] note in
            self?.routeChanged(note)
        }
        center.addObserver(forName: AVAudioSession.mediaServicesWereResetNotification, object: session, queue: .main) { [weak self] _ in
            self?.servicesReset()
        }
        setUpRemote()
    }

    // MARK: What screens, the system and the self-test call

    func play(items: [Int64], start: Int, done: @escaping () -> Void = {}) {
        startPlaying("play", done, { self.source = nil }) { try $0.play(items: items, start: UInt32(start), autoplay: true) }
    }

    func playCollection(id: Int64, startEntry: Int64?, done: @escaping () -> Void = {}) {
        startPlaying("play a playlist", done, { self.source = id }) {
            try $0.playCollection(collectionId: id, startEntry: startEntry, autoplay: true)
        }
    }

    func resume(done: @escaping () -> Void = {}) {
        startPlaying("resume", done) { try $0.resume() }
    }

    /// A pause the person asked for (also by a system button): no playing on after a call.
    func pause(done: @escaping () -> Void = {}) {
        forgetInterruption()
        command("pause", done) { try $0.pause() }
    }

    func toggle() {
        if state?.wantsPlay == true { pause() } else { resume() }
    }

    func next(done: @escaping () -> Void = {}) {
        command("next", done) { try $0.next() }
    }

    /// Plays the entry with this key (from the queue on the player screen).
    func jump(key: Int64, done: @escaping () -> Void = {}) {
        startPlaying("jump", done) {
            try $0.jump(entryKey: key)
            try $0.resume()
        }
    }

    func remove(key: Int64, done: @escaping () -> Void = {}) {
        command("remove", done) { try $0.remove(entryKey: key) }
    }

    func moveEntry(key: Int64, to: Int, done: @escaping () -> Void = {}) {
        command("move", done) { try $0.moveEntry(entryKey: key, to: UInt32(max(0, to))) }
    }

    func playNext(key: Int64, done: @escaping () -> Void = {}) {
        command("play next", done) { try $0.playNext(entryKey: key) }
    }

    /// Adds an item (a song from a playlist) at the end; to an empty queue, it plays.
    func add(itemId: Int64, done: @escaping () -> Void = {}) {
        startPlaying("add", done) { try $0.add(items: [itemId]) }
    }

    /// Adds an item right after what plays; to an empty queue, it plays.
    func addNext(itemId: Int64, done: @escaping () -> Void = {}) {
        startPlaying("play next", done) {
            try $0.add(items: [itemId])
            // A new entry goes at the end of the list (also when shuffled).
            if let entry = $0.entries().entries.last { try $0.playNext(entryKey: entry.key) }
        }
    }

    /// A screen showing the queue's entries calls this with true while it shows (and false
    /// after): only then are thousands of entries read again on every change.
    func wantEntries(_ on: Bool) {
        entryScreens = max(0, entryScreens + (on ? 1 : -1))
        fetchEntries()
    }

    private func fetchEntries() {
        guard let revision = state?.entriesRevision, entryScreens > 0,
              entries?.revision != revision, fetching != revision
        else { return }
        fetching = revision
        commands.async {
            let got = try? self.made().entries()
            DispatchQueue.main.async {
                self.fetching = nil
                if let got, got.revision >= (self.entries?.revision ?? 0) { self.entries = got }
                // Changed again meanwhile: once more.
                self.fetchEntries()
            }
        }
    }

    /// Starts the entry over; within its first 3 s, goes to the one before.
    func previous(done: @escaping () -> Void = {}) {
        command("previous", done) { try $0.previous() }
    }

    func seek(positionMs: Int64, done: @escaping () -> Void = {}) {
        command("seek", done) { try $0.seek(positionMs: positionMs) }
    }

    func setRepeat(_ mode: RepeatMode, done: @escaping () -> Void = {}) {
        command("repeat", done) { try $0.setRepeat(repeat: mode) }
    }

    func setShuffle(_ shuffle: Bool, done: @escaping () -> Void = {}) {
        command("shuffle", done) { try $0.setShuffle(shuffle: shuffle) }
    }

    func setSpeed(_ speed: Double, done: @escaping () -> Void = {}) {
        command("speed", done) { try $0.setSpeed(speed: speed) }
    }

    /// Ends playback and empties the queue (it leaves the lock screen).
    func stop(done: @escaping () -> Void = {}) {
        forgetInterruption()
        command("stop", { self.source = nil; done() }) { try $0.stop() }
    }

    /// What the system's buttons do, by name (the self-test presses them by name too).
    let remoteActions: [String: (Playback) -> Void] = [
        "play": { $0.resume() },
        "pause": { $0.pause() },
        "toggle": { $0.toggle() },
        "next": { $0.next() },
        "previous": { $0.previous() },
        "stop": { $0.stop() },
    ]

    // MARK: The queue

    /// Starts playback: the audio session first (refused during a phone call).
    private func startPlaying(_ name: String, _ done: @escaping () -> Void, _ worked: @escaping () -> Void = {},
                              _ call: @escaping (CoreQueue) throws -> Void) {
        guard activate() else {
            message = "Another app is using the sound (a call?). Try again after it."
            done()
            return
        }
        keepGoing(name, done, worked, call)
    }

    /// A command after which playback goes on.
    private func keepGoing(_ name: String, _ done: @escaping () -> Void, _ worked: @escaping () -> Void = {},
                           _ call: @escaping (CoreQueue) throws -> Void) {
        starting += 1
        command(name, {
            self.starting -= 1
            self.settle()
            self.changed()
            done()
        }, worked, call)
    }

    /// Runs `call` on the queue off the main thread (making the queue first if needed). Then,
    /// on the main thread: `worked` if it did, `message` says why if not, and `done` always.
    private func command(_ name: String, _ done: @escaping () -> Void, _ worked: @escaping () -> Void = {},
                         _ call: @escaping (CoreQueue) throws -> Void) {
        commands.async {
            var failed: String?
            do {
                try call(self.made())
            } catch {
                failed = describe(error)
                print("Elsewhere: \(name) failed: \(failed!)")
            }
            DispatchQueue.main.async {
                self.message = failed
                if failed == nil { worked() }
                done()
            }
        }
    }

    private func made() throws -> CoreQueue {
        if let queue { return queue }
        let made = try CoreQueue()
        made.setListener(listener: QueueFeed())
        queue = made
        drawLock.lock()
        drawing = made
        drawLock.unlock()
        return made
    }

    fileprivate func publish(_ new: QueueState) {
        if let old = state, old.revision >= new.revision { return }
        state = new
        settle()
        changed()
    }

    /// Done playing (the end, or nothing queued): other apps' audio may go on. Not while a
    /// play is on its way: the queue's first state is empty, and comes after the activation.
    private func settle() {
        guard let state, starting == 0, !resumeAfterInterruption, !state.wantsPlay,
              state.finished || state.entryCount == 0
        else { return }
        deactivate()
    }

    private func changed() {
        fetchEntries()
        showRemote()
        showNowPlaying()
        if let state { SelfTest.onQueue(state) }
    }

    private func forgetInterruption() {
        guard resumeAfterInterruption else { return }
        resumeAfterInterruption = false
        changed()
    }

    // MARK: Video (VideoView)

    /// A video view came on screen: it gets the frames (main thread).
    func attachVideo(_ view: VideoFrames) {
        videoViews.removeAll { $0 === view }
        videoViews.append(view)
        command("show video", {}) { try $0.attachFrames(listener: view) }
    }

    /// A video view went: the one before it gets the frames, or video goes off (the sound
    /// plays on).
    func detachVideo(_ view: VideoFrames) {
        guard let at = videoViews.firstIndex(where: { $0 === view }) else { return }
        let wasNewest = at == videoViews.count - 1
        videoViews.remove(at: at)
        guard wasNewest else { return }
        if let newest = videoViews.last {
            command("show video", {}) { try $0.attachFrames(listener: newest) }
        } else {
            command("hide video", {}) { try $0.detachVideo() }
        }
    }

    /// Draws the current frame into memory at `address` (on the drawing thread).
    func drawFrame(width: Int, height: Int, stride: Int, address: UInt64) -> Bool {
        drawLock.lock()
        let queue = drawing
        drawLock.unlock()
        guard let queue else { return false }
        return (try? queue.drawFrame(width: UInt32(width), height: UInt32(height), stride: UInt32(stride), address: address)) ?? false
    }

    // MARK: The audio session

    private func activate() -> Bool {
        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(.playback, mode: .default)
            try session.setActive(true)
            sessionActive = true
            return true
        } catch {
            print("Elsewhere: no audio session (a call?), not playing: \(error)")
            return false
        }
    }

    /// Done playing: other apps' audio (a podcast) may go on.
    private func deactivate() {
        guard sessionActive else { return }
        sessionActive = false
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    private func interrupted(_ note: Notification) {
        guard let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
              let type = AVAudioSession.InterruptionType(rawValue: raw)
        else { return }
        switch type {
        case .began:
            // iOS has stopped the sound already; the queue pauses where it is.
            guard state?.wantsPlay == true || starting > 0 else { return }
            resumeAfterInterruption = true
            command("pause for an interruption", { self.changed() }) { try $0.pause() }
        case .ended:
            guard resumeAfterInterruption else { return }
            resumeAfterInterruption = false
            let options = AVAudioSession.InterruptionOptions(
                rawValue: note.userInfo?[AVAudioSessionInterruptionOptionKey] as? UInt ?? 0)
            // Without shouldResume (the person played something else meanwhile) it stays paused.
            if options.contains(.shouldResume) && activate() {
                keepGoing("resume after an interruption", {}) { try $0.resume() }
            } else {
                changed()
            }
        @unknown default:
            break
        }
    }

    private func routeChanged(_ note: Notification) {
        guard let raw = note.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt,
              AVAudioSession.RouteChangeReason(rawValue: raw) == .oldDeviceUnavailable
        else { return }
        // Headphones unplugged or Bluetooth gone: pause before the sound comes out of the speaker.
        print("Elsewhere: headphones gone, pausing")
        pause()
    }

    /// iOS restarted its media services: every audio object is new, so set them up again.
    private func servicesReset() {
        print("Elsewhere: media services were reset")
        sessionActive = false
        let going = keepsGoing
        if going { _ = activate() }
        setUpRemote()
        shownInfo = nil
        command("reload audio", { self.changed() }) { try $0.reloadAudio() }
    }

    // MARK: The lock screen and Control Center

    private func setUpRemote() {
        let center = MPRemoteCommandCenter.shared()
        let buttons: [(MPRemoteCommand, String)] = [
            (center.playCommand, "play"),
            (center.pauseCommand, "pause"),
            (center.togglePlayPauseCommand, "toggle"),
            (center.nextTrackCommand, "next"),
            (center.previousTrackCommand, "previous"),
            (center.stopCommand, "stop"),
        ]
        for (button, name) in buttons {
            button.removeTarget(nil)
            button.addTarget { [weak self] _ in
                guard let self, self.state?.current != nil, let action = self.remoteActions[name] else {
                    return .noActionableNowPlayingItem
                }
                action(self)
                return .success
            }
        }
        center.changePlaybackPositionCommand.removeTarget(nil)
        center.changePlaybackPositionCommand.addTarget { [weak self] event in
            guard let self, self.state?.current != nil,
                  let event = event as? MPChangePlaybackPositionCommandEvent
            else { return .noActionableNowPlayingItem }
            self.seek(positionMs: Int64((event.positionTime * 1000).rounded()))
            return .success
        }
        // Buttons the queue has nothing for stay off.
        for unused in [
            center.skipForwardCommand, center.skipBackwardCommand, center.seekForwardCommand,
            center.seekBackwardCommand, center.changeRepeatModeCommand, center.changeShuffleModeCommand,
            center.changePlaybackRateCommand, center.ratingCommand, center.likeCommand,
            center.dislikeCommand, center.bookmarkCommand, center.enableLanguageOptionCommand,
            center.disableLanguageOptionCommand,
        ] {
            unused.isEnabled = false
        }
        showRemote()
    }

    private func showRemote() {
        let center = MPRemoteCommandCenter.shared()
        let queued = state?.current != nil
        center.nextTrackCommand.isEnabled = state?.hasNext == true
        center.previousTrackCommand.isEnabled = queued
        center.changePlaybackPositionCommand.isEnabled = queued && (state?.playback.durationMs ?? 0) > 0
    }

    /// What was last handed to the lock screen, to hand it over only when it would be wrong.
    private struct ShownInfo {
        var now: NowPlaying?
        var duration: Int64?
        var cover: UIImage?
        var index: UInt32
        var count: Int
        var rate: Double
        var elapsed: Double
        var at: TimeInterval
    }
    private var shownInfo: ShownInfo?

    private func showNowPlaying() {
        let center = MPNowPlayingInfoCenter.default()
        guard let state, let index = state.current, index < state.entryCount else {
            if center.nowPlayingInfo != nil { center.nowPlayingInfo = nil }
            shownInfo = nil
            return
        }
        let now = state.nowPlaying
        showCover(now?.cover)
        let playback = state.playback
        // The lock screen moves the position on by itself while the rate is not 0.
        let rate = playback.status == .playing && state.wantsPlay ? playback.speed : 0
        let elapsed = Double(playback.positionMs) / 1000
        let time = ProcessInfo.processInfo.systemUptime
        if let shown = shownInfo, shown.now == now, shown.duration == playback.durationMs,
           shown.cover === cover, shown.index == index, shown.count == Int(state.entryCount),
           shown.rate == rate {
            let off = abs(shown.elapsed + (time - shown.at) * shown.rate - elapsed)
            if rate == 0 ? off == 0 : off < 1 { return }
        }
        var info: [String: Any] = [
            MPMediaItemPropertyTitle: now?.title ?? "",
            MPNowPlayingInfoPropertyElapsedPlaybackTime: elapsed,
            MPNowPlayingInfoPropertyPlaybackRate: rate,
            MPNowPlayingInfoPropertyDefaultPlaybackRate: 1.0,
            MPNowPlayingInfoPropertyPlaybackQueueIndex: Int(index),
            MPNowPlayingInfoPropertyPlaybackQueueCount: Int(state.entryCount),
            MPNowPlayingInfoPropertyMediaType: (now?.media == .video ? MPNowPlayingInfoMediaType.video : .audio).rawValue,
        ]
        if let artist = now?.artist { info[MPMediaItemPropertyArtist] = artist }
        if let album = now?.album { info[MPMediaItemPropertyAlbumTitle] = album }
        if let duration = playback.durationMs, duration > 0 {
            info[MPMediaItemPropertyPlaybackDuration] = Double(duration) / 1000
        }
        if let cover {
            info[MPMediaItemPropertyArtwork] = MPMediaItemArtwork(boundsSize: cover.size) { _ in cover }
        }
        center.nowPlayingInfo = info
        shownInfo = ShownInfo(now: now, duration: playback.durationMs, cover: cover, index: index,
                              count: Int(state.entryCount), rate: rate, elapsed: elapsed, at: time)
    }

    // MARK: The cover

    /// Covers on the lock screen are scaled to this many pixels at most.
    private static let coverSize: CGFloat = 512
    private var coverPath: String?
    private var cover: UIImage?

    /// Loads the current entry's cover off the main thread; shows it once loaded.
    private func showCover(_ path: String?) {
        guard path != coverPath else { return }
        coverPath = path
        cover = nil
        guard let path else { return }
        DispatchQueue.global(qos: .utility).async {
            let loaded = Playback.loadCover(path, size: Playback.coverSize)
            DispatchQueue.main.async {
                guard self.coverPath == path, let loaded else { return }
                self.cover = loaded
                self.showNowPlaying()
            }
        }
    }

    /// The image at `path` scaled down to `size` pixels at most, or nil if it does not load
    /// (a cover can be a 4000-pixel photo).
    static func loadCover(_ path: String, size: CGFloat) -> UIImage? {
        guard let image = UIImage(contentsOfFile: path), image.size.width > 0, image.size.height > 0 else { return nil }
        let scale = min(1, size / max(image.size.width, image.size.height))
        if scale >= 1 { return image }
        let size = CGSize(width: max(1, (image.size.width * scale).rounded()), height: max(1, (image.size.height * scale).rounded()))
        let format = UIGraphicsImageRendererFormat.default()
        format.scale = 1
        return UIGraphicsImageRenderer(size: size, format: format).image { _ in
            image.draw(in: CGRect(origin: .zero, size: size))
        }
    }

    // MARK: For the self-test

    /// What the lock screen shows now, as one line.
    var nowPlayingLine: String {
        guard let info = MPNowPlayingInfoCenter.default().nowPlayingInfo else { return "shown=false" }
        let title = info[MPMediaItemPropertyTitle] as? String ?? "null"
        let rate = info[MPNowPlayingInfoPropertyPlaybackRate] as? Double ?? -1
        let duration = info[MPMediaItemPropertyPlaybackDuration] as? Double ?? -1
        let index = info[MPNowPlayingInfoPropertyPlaybackQueueIndex] as? Int ?? -1
        let next = MPRemoteCommandCenter.shared().nextTrackCommand.isEnabled
        return "shown=true rate=\(rate) duration=\(duration) index=\(index) next=\(next) title=\(title)"
    }
}

/// Hands the queue's states to the main thread.
private final class QueueFeed: QueueListener {
    func onState(state: QueueState) {
        DispatchQueue.main.async { Playback.shared.publish(state) }
    }
}
