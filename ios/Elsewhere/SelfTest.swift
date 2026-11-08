import Foundation

/// The CI check of the job path through the real app (scripts/selftest_ios.sh). Debug
/// builds only: every job state the store receives is printed as one line, and the launch
/// argument `-selftest <mode>` makes the app act at launch: demo, resume (every Interrupted
/// job), discard (every Interrupted job) or ytdlp.
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

    private static func say(_ line: String) {
        print("ElsewhereJobs: \(line)")
        fflush(stdout)
    }
}
