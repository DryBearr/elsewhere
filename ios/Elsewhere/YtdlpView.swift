import SwiftUI

/// yt-dlp in the Rust core: started once, then used by YtdlpView.
@MainActor
final class Ytdlp: ObservableObject {
    static let shared = Ytdlp()

    @Published private(set) var status = "Starting yt-dlp…"
    @Published private(set) var ready = false
    private var started = false

    /// Starts Python and yt-dlp, which sit inside the app bundle (nothing to unpack on iOS).
    func start() {
        guard !started else { return }
        started = true
        Task.detached {
            let folders = Folders(
                data: AppFolders.data.path,
                cache: AppFolders.cache.path,
                temp: NSTemporaryDirectory(),
                pythonHome: Bundle.main.bundleURL.appendingPathComponent("python").path,
                programs: nil
            )
            var text: String
            var ok = false
            do {
                text = "yt-dlp \(try startYtdlp(folders: folders)) is ready."
                ok = true
            } catch {
                text = "yt-dlp could not start: \(describe(error))"
            }
            // The simulator check in .github/workflows/mobile.yml looks for this line.
            print("Elsewhere: \(text)")
            let (status, ready) = (text, ok)
            await MainActor.run {
                self.status = status
                self.ready = ready
            }
        }
    }

    /// Title, uploader and length of a link, or the error. Blocks; call it off the main thread.
    nonisolated static func lookUp(_ link: String) -> String {
        do {
            let json = try ytdlpInfo(url: link, args: ["--playlist-items", "1"], listener: NoEvents(), cancel: CancelToken())
            let info = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any] ?? [:]
            let lines = ["title", "uploader", "duration_string"].compactMap { info[$0] as? String }
            return lines.isEmpty ? "No information found." : lines.joined(separator: "\n")
        } catch {
            return describe(error)
        }
    }
}

private final class NoEvents: EventListener {
    func onEvent(event: YtdlpEvent) {}
}

/// Paste a link, get its title: shows that yt-dlp works on this phone.
struct YtdlpView: View {
    @ObservedObject private var ytdlp = Ytdlp.shared
    @State private var link = ""
    @State private var busy = false
    @State private var result = ""

    var body: some View {
        Section("yt-dlp") {
            Text(ytdlp.status)
            TextField("Video link", text: $link)
                .keyboardType(.URL)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .disabled(!ytdlp.ready)
            Button("Get info", action: lookUp)
                .disabled(!ytdlp.ready || busy || link.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            if !result.isEmpty {
                Text(result).textSelection(.enabled)
            }
        }
        .onAppear { ytdlp.start() }
    }

    private func lookUp() {
        busy = true
        result = "Looking it up…"
        let url = link.trimmingCharacters(in: .whitespacesAndNewlines)
        Task.detached {
            let text = Ytdlp.lookUp(url)
            await MainActor.run {
                result = text
                busy = false
            }
        }
    }
}
