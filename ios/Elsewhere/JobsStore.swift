import Foundation

/// The one place the app keeps job states. The core sends a job's whole state on every
/// change; this keeps the newest revision of each job and every screen reads it from here.
/// No screen keeps its own copy. Changed on the main thread only.
final class JobsStore: ObservableObject {
    static let shared = JobsStore()

    @Published private(set) var jobs: [Int64: JobState] = [:]

    /// Newest first, for lists.
    var newestFirst: [JobState] {
        jobs.values.sorted { $0.id > $1.id }
    }

    func apply(_ job: JobState) {
        if let old = jobs[job.id], old.revision >= job.revision {
            return
        }
        jobs[job.id] = job
        SelfTest.onJob(job)
    }

    func cancel(_ id: Int64) { act("cancel") { _ = try cancelJob(id: id) } }
    func resume(_ id: Int64) { act("resume") { try resumeJob(id: id) } }
    func discard(_ id: Int64) { act("discard") { _ = try discardJob(id: id) } }

    /// Removes a finished job. The core sends nothing for a removed job, so drop it here.
    func forget(_ id: Int64) {
        Task.detached {
            if (try? forgetJob(id: id)) == true {
                await MainActor.run { _ = JobsStore.shared.jobs.removeValue(forKey: id) }
            }
        }
    }

    private func act(_ name: String, _ call: @escaping () throws -> Void) {
        Task.detached {
            do {
                try call()
            } catch {
                print("Elsewhere: \(name) failed: \(describe(error))")
            }
        }
    }
}

/// Receives states on the core's delivery thread and hands them to the main thread, in
/// the order they came (one serial queue keeps the order).
final class JobsFeed: JobListener {
    func onJob(job: JobState) {
        DispatchQueue.main.async { JobsStore.shared.apply(job) }
    }
}

extension JobStatus {
    var isFinished: Bool { self == .done || self == .failed || self == .cancelled }
}
