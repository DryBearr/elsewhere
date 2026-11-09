import SwiftUI

/// The Music tab: saved playlists; open one to see its songs and play them.
struct MusicView: View {
    @ObservedObject private var jobs = JobsStore.shared
    @State private var loaded: Loaded<[CollectionSummary]> = .loading

    /// Saving a playlist changes what is here: look again when a job ends.
    private var ended: Int { jobs.jobs.values.filter { $0.status.isFinished }.count }

    var body: some View {
        List {
            switch loaded {
            case .loading:
                EmptyView()
            case .failed(let text):
                ProblemRow(text: text) { Task { await load() } }
            case .done(let playlists) where playlists.isEmpty:
                Text("No playlists yet. Save one from the Add tab.").foregroundStyle(.secondary)
            case .done(let playlists):
                ForEach(playlists, id: \.id) { playlist in
                    NavigationLink {
                        PlaylistView(id: playlist.id, name: playlist.name)
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(playlist.name).lineLimit(2)
                            Text(songs(playlist.entries)).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
            }
        }
        .navigationTitle("Music")
        .task(id: ended) { await load() }
        .refreshable { await load() }
    }

    private func load() async {
        loaded = await Task.detached { () -> Loaded<[CollectionSummary]> in
            do {
                return .done(try musicPlaylists())
            } catch {
                return .failed(describe(error))
            }
        }.value
    }
}

/// Something loaded off the main thread.
enum Loaded<T> {
    case loading
    case failed(String)
    case done(T)
}

/// The Music tab's playlists (smart ones have no songs yet, so they are not shown).
private func musicPlaylists() throws -> [CollectionSummary] {
    if let error = Core.startError { throw ShownError(text: "The core did not start: \(error)") }
    guard let music = try categories().first(where: { $0.name == "Music" && $0.media == .audio }) else { return [] }
    return try collections(categoryId: music.id).filter { !$0.smart }
}

struct ShownError: Error, CustomStringConvertible {
    let text: String
    var description: String { text }
}

/// One playlist: its songs, page by page; tap one to play from there.
struct PlaylistView: View {
    let id: Int64
    let name: String

    /// Entries asked for per page.
    private static let page: UInt32 = 100

    @ObservedObject private var playback = Playback.shared
    @ObservedObject private var jobs = JobsStore.shared
    @State private var summary: Loaded<CollectionSummary> = .loading
    @State private var rows: [CollectionEntry] = []
    @State private var next: String?
    @State private var complete = false
    @State private var loading = false
    @State private var failed: String?
    /// Bumped to start over: a job ended (the playlist may have changed) or Retry.
    @State private var round = 0

    private var ended: Int { jobs.jobs.values.filter { $0.status.isFinished }.count }

    var body: some View {
        List {
            switch summary {
            case .loading:
                EmptyView()
            case .failed(let text):
                ProblemRow(text: text) { round += 1 }
            case .done(let playlist):
                HStack {
                    Text(songs(playlist.entries)).foregroundStyle(.secondary)
                    Spacer()
                    Button {
                        playback.playCollection(id: id, startEntry: nil)
                    } label: {
                        Label("Play", systemImage: "play.fill")
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(playlist.entries == 0)
                    .accessibilityIdentifier("play-all")
                }
                ForEach(Array(rows.enumerated()), id: \.element.entryId) { index, row in
                    SongRow(index: index, row: row, playlist: id)
                        .onAppear { if index == rows.count - 1 { Task { await more() } } }
                }
                if let failed {
                    ProblemRow(text: failed) { Task { await more() } }
                }
            }
        }
        .navigationTitle(name)
        .task(id: "\(ended)-\(round)") { await reload() }
    }

    private func reload() async {
        rows = []
        next = nil
        complete = false
        failed = nil
        let id = id
        summary = await Task.detached { () -> Loaded<CollectionSummary> in
            do {
                return .done(try collection(id: id))
            } catch {
                return .failed(describe(error))
            }
        }.value
        await more()
    }

    private func more() async {
        guard !loading, !complete, case .done = summary else { return }
        loading = true
        defer { loading = false }
        let id = id
        let after = next
        do {
            let page = try await Task.detached {
                try collectionItems(collectionId: id, after: after, limit: PlaylistView.page)
            }.value
            rows += page.entries
            next = page.next
            complete = page.next == nil
            failed = nil
        } catch {
            failed = describe(error)
        }
    }
}

/// A song of a playlist: bold while it plays, dimmed when its file is gone.
private struct SongRow: View {
    let index: Int
    let row: CollectionEntry
    let playlist: Int64
    @ObservedObject private var playback = Playback.shared

    var body: some View {
        let item = row.item
        let here = playback.source == playlist && playback.state?.nowPlaying?.itemId == item.id
        let skipped = playback.source == playlist && (playback.state?.skipped.contains { $0.itemId == item.id } ?? false)
        let note: String = {
            if item.missing { return "File missing" }
            if skipped { return "Could not play" }
            return [item.artist, item.durationMs.map(time)].compactMap { $0 }.joined(separator: " · ")
        }()
        Button {
            playback.playCollection(id: playlist, startEntry: row.entryId)
        } label: {
            VStack(alignment: .leading, spacing: 2) {
                Text(item.title)
                    .lineLimit(2)
                    .fontWeight(here ? .bold : .regular)
                    .foregroundStyle(here ? Color.accentColor : Color.primary)
                if !note.isEmpty {
                    Text(note).font(.caption).foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("song-\(index)")
        .contextMenu {
            if !item.missing {
                Button("Play next") { playback.addNext(itemId: item.id) }
                Button("Add to queue") { playback.add(itemId: item.id) }
            }
        }
        .disabled(item.missing)
        .opacity(item.missing ? 0.4 : 1)
    }
}

/// A failed load, with Retry.
struct ProblemRow: View {
    let text: String
    let retry: () -> Void

    var body: some View {
        VStack(alignment: .leading) {
            Text(text).foregroundStyle(.red)
            Button("Retry", action: retry)
        }
    }
}

private func songs(_ count: Int64) -> String {
    count == 1 ? "1 song" : "\(count) songs"
}

/// 3:07, or 1:02:03 from an hour.
private func time(_ ms: Int64) -> String {
    let seconds = ms / 1000
    let (hours, minutes, rest) = (seconds / 3600, seconds / 60 % 60, seconds % 60)
    return hours > 0 ? String(format: "%lld:%02lld:%02lld", hours, minutes, rest) : String(format: "%lld:%02lld", minutes, rest)
}
