import Foundation

/// The Rust core. Calls into it block, so screens make them off the main thread
/// (`Task.detached`).
enum Core {
    /// Why the core did not start, if it did not.
    private(set) static var startError: String?

    /// Starts the core: folders, database, jobs. Cheap, so it runs at app start; it must run
    /// first because it sets environment variables. Python and yt-dlp, which sit inside the
    /// app bundle, start later on first use.
    static func boot() {
        let files = FileManager.default
        let data = files.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        let cache = files.urls(for: .cachesDirectory, in: .userDomainMask)[0]
        // Documents is visible in the Files app (UIFileSharingEnabled in project.yml).
        let media = files.urls(for: .documentDirectory, in: .userDomainMask)[0].appendingPathComponent("Media")
        let folders = Folders(
            data: data.path,
            cache: cache.path,
            temp: NSTemporaryDirectory(),
            media: media.path,
            pythonHome: Bundle.main.bundleURL.appendingPathComponent("python").path,
            pythonPaths: [],
            programs: nil
        )
        do {
            try start(folders: folders, pythonBundle: nil)
            try setJobListener(listener: JobsFeed())
        } catch {
            startError = describe(error)
            print("Elsewhere: the core did not start: \(startError!)")
        }
    }

    /// True once the core has a broken part it could not heal: only a fresh start helps.
    /// iOS apps cannot restart themselves, so the app has to ask the person to reopen it.
    /// Nothing is lost: jobs that were running come back Interrupted, with Resume.
    static var needsRestart: Bool {
        coreHealth().needsRestart
    }
}
