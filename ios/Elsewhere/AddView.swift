import SwiftUI

/// Save a playlist (or one video): link, audio or video, quality, lyrics.
struct AddView: View {
    let onStarted: () -> Void
    @State private var link = ""
    @State private var audio = true
    @State private var quality: UInt32? = 720
    @State private var lyrics = true
    @State private var error: String?

    private let qualities: [UInt32?] = [nil, 1080, 720, 480, 360]

    var body: some View {
        Form {
            Section("Save a playlist or video") {
                TextField("YouTube or YouTube Music link", text: $link)
                    .keyboardType(.URL)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                Picker("Format", selection: $audio) {
                    Text("Audio (m4a)").tag(true)
                    Text("Video (mp4)").tag(false)
                }
                .pickerStyle(.segmented)
                if !audio {
                    Picker("Quality", selection: $quality) {
                        ForEach(qualities, id: \.self) { height in
                            Text(height.map { "\($0)p" } ?? "Best").tag(height)
                        }
                    }
                }
                Toggle("Find lyrics", isOn: $lyrics)
                Button("Save") {
                    let options = PlaylistOptions(format: audio ? .audio : .video(maxHeight: quality), lyrics: lyrics, limit: nil)
                    let link = link
                    run { _ = try savePlaylist(link: link, options: options) }
                }
                .disabled(link.trimmingCharacters(in: .whitespaces).isEmpty)
                if let error {
                    Text(error).foregroundStyle(.red)
                }
            }
            Section("Check downloads without network") {
                Button("Run a demo job") {
                    run { _ = try startDemoJob(options: DemoOptions(items: 10, itemMs: 400, failAt: 3, warnAt: 2)) }
                }
            }
        }
        .navigationTitle("Add")
    }

    private func run(_ call: @escaping () throws -> Void) {
        error = nil
        Task.detached {
            do {
                try call()
                await MainActor.run { onStarted() }
            } catch {
                let message = describe(error)
                await MainActor.run { self.error = message }
            }
        }
    }
}
