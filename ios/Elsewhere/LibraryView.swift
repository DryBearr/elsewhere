import SwiftUI

/// The library's tabs, and what the core has loaded.
struct LibraryView: View {
    @State private var names: [String] = []
    @State private var status = ""

    var body: some View {
        List {
            Section("Library") {
                ForEach(names, id: \.self) { Text($0) }
            }
            Section {
                Text(status).font(.caption).foregroundStyle(.secondary)
            }
        }
        .navigationTitle("Library")
        .task {
            names = await Task.detached { ((try? categories()) ?? []).map(\.name) }.value
            status = await Task.detached { () -> String in
                if let error = Core.startError { return "The core did not start: \(error)" }
                return "yt-dlp " + (coreStatus().ytdlpStarted ? "running" : "starts with the first download")
            }.value
        }
    }
}
