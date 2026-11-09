import AVFoundation
import Foundation

/// The CI check of the job path through the real app (scripts/selftest_ios.sh). Debug
/// builds only: every job state the store receives is printed as one line, and the launch
/// argument `-selftest <mode>` makes the app act at launch: demo, long-demo, resume (every Interrupted
/// job), discard (every Interrupted job), ytdlp, player (plays the test files in the
/// app's `media` folder through the real engine and sound output), or background (plays two
/// test songs again and again through Playback; Darwin notifications then press its system
/// buttons and play interruptions, unplugged headphones and a media services reset at it),
/// or music (saves the UI tests' playlist from `-server <url>`, scripts/selftest_server.py,
/// through yt-dlp like the Add tab does; ios/ElsewhereUITests then plays it).
enum SelfTest {
    #if DEBUG
    static let enabled = true
    #else
    static let enabled = false
    #endif

    static func run(arguments: [String]) {
        guard enabled, let flag = arguments.firstIndex(of: "-selftest"), flag + 1 < arguments.count else { return }
        let mode = arguments[flag + 1]
        listen()
        Task.detached {
            do {
                switch mode {
                case "demo":
                    // 20 items of 4 s: still running when killed, even when the log arrives late.
                    _ = try startDemoJob(options: DemoOptions(items: 20, itemMs: 4000, failAt: 3, warnAt: 2))
                case "long-demo":
                    // Runs for minutes: it is still running when the app is killed.
                    _ = try startDemoJob(options: DemoOptions(items: 200, itemMs: 1000, failAt: nil, warnAt: nil))
                case "resume":
                    for job in try interrupted() { try resumeJob(id: job.id) }
                case "discard":
                    for job in try interrupted() { _ = try discardJob(id: job.id) }
                case "ytdlp":
                    say("YTDLP \(try ytdlpVersion())")
                case "player":
                    try await player()
                case "background":
                    try background()
                case "music":
                    let at = arguments.firstIndex(of: "-server").map { $0 + 1 }
                    let server = at.flatMap { $0 < arguments.count ? arguments[$0] : nil } ?? "http://127.0.0.1:8765"
                    try music(server: server)
                default:
                    break
                }
            } catch {
                say("ERROR \(mode): \(describe(error))")
            }
        }
    }

    /// One line per state; for a final state also whether the store equals the core.
    static func onJob(_ state: JobState) {
        guard enabled else { return }
        say("STATE id=\(state.id) rev=\(state.revision) status=\(state.status) done=\(state.done) failed=\(state.failed) total=\(state.total.map { "\($0)" } ?? "null")")
        guard state.status.isFinished else { return }
        let shown = JobsStore.shared.jobs[state.id]
        Task.detached {
            let core = try? job(id: state.id)
            say("FINAL id=\(state.id) status=\(state.status) done=\(state.done) failed=\(state.failed) log=\(state.logLines) match=\(shown == core)")
        }
    }

    private static func interrupted() throws -> [JobState] {
        try jobs().filter { $0.status == .interrupted }
    }

    /// Players of the self-test, kept so they live until the process ends.
    nonisolated(unsafe) private static var players: [CorePlayer] = []

    /// Copies two test files into the media folder and plays each to its end, printing
    /// every state; then checks that a file outside the media folder is refused.
    private static func player() async throws {
        let files = FileManager.default
        let folder = Core.media.appendingPathComponent("selftest")
        try files.createDirectory(at: folder, withIntermediateDirectories: true)
        let player = try CorePlayer()
        players.append(player)
        player.setListener(listener: PlayerLog())
        for name in ["song.m4a", "clip.webm"] {
            let file = folder.appendingPathComponent(name)
            try? files.removeItem(at: file)
            try files.copyItem(at: Bundle.main.bundleURL.appendingPathComponent("media/\(name)"), to: file)
            let started = Date()
            try player.open(path: file.path, startMs: 0, autoplay: true)
            while ![.ended, .failed].contains(player.state().status) && Date().timeIntervalSince(started) < 30 {
                try await Task.sleep(nanoseconds: 20_000_000)
            }
            let state = player.state()
            let took = Int(Date().timeIntervalSince(started) * 1000)
            say("PLAYED file=\(name) status=\(state.status) pos=\(state.positionMs) dur=\(state.durationMs.map { "\($0)" } ?? "null") took=\(took) problem=\(state.problem?.detail ?? "null")")
        }
        do {
            try player.open(path: "/etc/hosts", startMs: 0, autoplay: true)
            say("PLAYER refused=no")
        } catch ApiError.Failed(let problem) {
            say("PLAYER refused=\(problem.kind)")
        }
    }

    /// One line per playlist player state.
    static func onQueue(_ state: QueueState) {
        guard enabled else { return }
        let playback = state.playback
        let current = state.current.map { "\($0)" } ?? "null"
        let item = state.nowPlaying.map { "\($0.itemId)" } ?? "null"
        let duration = playback.durationMs.map { "\($0)" } ?? "null"
        let problem = state.problem.map { "\($0.kind)" } ?? "null"
        say("QUEUE rev=\(state.revision) entries=\(state.entryCount) current=\(current) item=\(item) status=\(playback.status) wants=\(state.wantsPlay) pos=\(playback.positionMs) dur=\(duration) volume=\(playback.volume) finished=\(state.finished) next=\(state.hasNext) keeps=\(Playback.shared.keepsGoing) problem=\(problem) title=\(state.nowPlaying?.title ?? "null")")
    }

    /// Adds two test songs to the Music tab and plays them again and again through Playback.
    private static func background() throws {
        let files = FileManager.default
        guard let music = try categories().first(where: { $0.name == "Music" }) else {
            say("ERROR background: no Music tab")
            return
        }
        let folder = Core.media.appendingPathComponent("\(music.name)/selftest-background")
        try files.createDirectory(at: folder, withIntermediateDirectories: true)
        var items: [Int64] = []
        for name in ["long.m4a", "song.m4a"] {
            let file = folder.appendingPathComponent(name)
            try? files.removeItem(at: file)
            try files.copyItem(at: Bundle.main.bundleURL.appendingPathComponent("media/\(name)"), to: file)
            items.append(try addFile(tabId: music.id, path: file.path))
        }
        DispatchQueue.main.async {
            Playback.shared.play(items: items, start: 0) { Playback.shared.setRepeat(.all) }
        }
    }

    /// Saves the UI tests' playlist as songs (again is fine: only what is new is saved). A
    /// song file the test took away comes back first.
    private static func music(server: String) throws {
        let files = FileManager.default
        let music = Core.media.appendingPathComponent("Music")
        if let found = files.enumerator(at: music, includingPropertiesForKeys: nil) {
            for case let file as URL in found where file.pathExtension == "removed" {
                try? files.moveItem(at: file, to: file.deletingPathExtension())
            }
        }
        let songs = try savePlaylist(link: "\(server)/music-playlist.html",
                                     options: PlaylistOptions(format: .audio, lyrics: false, limit: nil))
        let videos = try savePlaylist(link: "\(server)/video-playlist.html",
                                      options: PlaylistOptions(format: .video(maxHeight: nil), lyrics: false, limit: nil))
        say("SAVING songs=\(songs) videos=\(videos)")
    }

    /// Takes the file of the UI tests' playlist's second song away (put back by `music`).
    private static func removeSecondSong() {
        Task.detached {
            do {
                guard let music = try categories().first(where: { $0.name == "Music" }),
                      let list = try collections(categoryId: music.id).first(where: { $0.name == "Music Playlist" })
                else {
                    say("ERROR remove-second-song: no Music Playlist")
                    return
                }
                let rows = try collectionItems(collectionId: list.id, after: nil, limit: 0).entries
                guard rows.count > 1 else {
                    say("ERROR remove-second-song: \(rows.count) songs")
                    return
                }
                let file = URL(fileURLWithPath: try itemFile(id: rows[1].item.id))
                try FileManager.default.moveItem(at: file, to: file.appendingPathExtension("removed"))
                say("REMOVED \(file.lastPathComponent)")
            } catch {
                say("ERROR remove-second-song: \(describe(error))")
            }
        }
    }

    /// Darwin notifications the self-test sends:
    /// `xcrun simctl spawn <udid> notifyutil -p io.github.drybearr.elsewhere.selftest.<action>`.
    private static let prefix = "io.github.drybearr.elsewhere.selftest."
    private static let actions = [
        "remote-play", "remote-pause", "remote-toggle", "remote-next", "remote-previous", "remote-stop",
        "interruption-began", "interruption-ended", "route-unplugged", "media-reset", "repeat-off", "report",
        "remove-second-song",
    ]

    private static func listen() {
        let center = CFNotificationCenterGetDarwinNotifyCenter()
        for action in actions {
            CFNotificationCenterAddObserver(center, nil, { _, _, name, _, _ in
                guard let name = name?.rawValue as String? else { return }
                let action = String(name.dropFirst(SelfTest.prefix.count))
                DispatchQueue.main.async { SelfTest.act(action) }
            }, (prefix + action) as CFString, nil, .deliverImmediately)
        }
    }

    /// What the self-test sent (main thread). Interruptions, route changes and the media
    /// services reset are posted as iOS posts them, to the same observers.
    private static func act(_ action: String) {
        let playback = Playback.shared
        let session = AVAudioSession.sharedInstance()
        let center = NotificationCenter.default
        switch action {
        case "interruption-began":
            center.post(name: AVAudioSession.interruptionNotification, object: session, userInfo: [
                AVAudioSessionInterruptionTypeKey: AVAudioSession.InterruptionType.began.rawValue,
            ])
        case "interruption-ended":
            center.post(name: AVAudioSession.interruptionNotification, object: session, userInfo: [
                AVAudioSessionInterruptionTypeKey: AVAudioSession.InterruptionType.ended.rawValue,
                AVAudioSessionInterruptionOptionKey: AVAudioSession.InterruptionOptions.shouldResume.rawValue,
            ])
        case "route-unplugged":
            center.post(name: AVAudioSession.routeChangeNotification, object: session, userInfo: [
                AVAudioSessionRouteChangeReasonKey: AVAudioSession.RouteChangeReason.oldDeviceUnavailable.rawValue,
            ])
        case "media-reset":
            center.post(name: AVAudioSession.mediaServicesWereResetNotification, object: session)
        case "repeat-off":
            playback.setRepeat(.off)
        case "remove-second-song":
            removeSecondSong()
        case "report":
            if let state = playback.state { onQueue(state) }
            say("NOWPLAYING \(playback.nowPlayingLine)")
        default:
            guard action.hasPrefix("remote-"), let press = playback.remoteActions[String(action.dropFirst(7))] else {
                say("ERROR unknown self-test action \(action)")
                return
            }
            press(playback)
        }
        say("ACTED \(action)")
    }

    fileprivate static func say(_ line: String) {
        print("ElsewhereJobs: \(line)")
        fflush(stdout)
    }
}

/// Prints every state of the self-test's player.
private final class PlayerLog: PlayerListener {
    func onState(state: PlayerState) {
        SelfTest.say("PLAYER file=\(state.file.map { ($0 as NSString).lastPathComponent } ?? "null") rev=\(state.revision) status=\(state.status) pos=\(state.positionMs) video=\(state.hasVideo) audio=\(state.hasAudio) problem=\(state.problem?.detail ?? "null")")
    }
}
