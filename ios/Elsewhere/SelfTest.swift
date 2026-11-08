import Foundation

/// The CI check of the job path through the real app (scripts/selftest_ios.sh). Debug
/// builds only: every job state the store receives is printed as one line, and the launch
/// argument `-selftest <mode>` makes the app act at launch: demo, resume (every Interrupted
/// job), discard (every Interrupted job), ytdlp, or player (plays the test files in the
/// app's `media` folder through the real engine and sound output).
enum SelfTest {
    #if DEBUG
    static let enabled = true
    #else
    static let enabled = false
    #endif

    static func run(arguments: [String]) {
        guard enabled, let flag = arguments.firstIndex(of: "-selftest"), flag + 1 < arguments.count else { return }
        let mode = arguments[flag + 1]
        Task.detached {
            do {
                switch mode {
                case "demo":
                    _ = try startDemoJob(options: DemoOptions(items: 20, itemMs: 500, failAt: 3, warnAt: 2))
                case "resume":
                    for job in try interrupted() { try resumeJob(id: job.id) }
                case "discard":
                    for job in try interrupted() { _ = try discardJob(id: job.id) }
                case "ytdlp":
                    say("YTDLP \(try ytdlpVersion())")
                case "player":
                    try await player()
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
