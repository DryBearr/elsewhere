import SwiftUI

/// What a tab shows under its search field.
enum LibraryPart: Hashable {
    case playlists, songs, artists, albums
}

/// A tab of playable things (Music, Videos): its playlists (saved ones and one's own),
/// everything in it, its artists (for videos: channels) and albums, and a search over all
/// of it. Open a playlist, artist or album to see what is in it and play it. `tab` is the
/// library tab's name.
struct MusicView: View {
    var tab = "Music"
    @ObservedObject private var edits = LibraryEdits.shared
    @ObservedObject private var jobs = JobsStore.shared
    @State private var part = LibraryPart.playlists
    @State private var query = ""
    @State private var tabId: Loaded<Int64?> = .loading
    @State private var naming = false
    @State private var newName = ""
    @State private var retry = 0

    private var music: Bool { tab == "Music" }
    private var noun: String { music ? "song" : "video" }
    private var parts: [LibraryPart] { music ? [.playlists, .songs, .artists, .albums] : [.playlists, .songs, .artists] }
    private var searching: Bool { !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty }

    var body: some View {
        VStack(spacing: 0) {
            ProblemBanner()
            switch tabId {
            case .loading:
                Spacer()
            case .failed(let text):
                List { ProblemRow(text: text) { retry += 1 } }
            case .done(let id):
                let context = LibraryContext(tab: tab, noun: noun, tabId: id)
                if searching {
                    SearchList(context: context, query: query)
                } else {
                    Picker("Show", selection: $part) {
                        ForEach(parts, id: \.self) { part in
                            Text(label(part)).tag(part)
                        }
                    }
                    .pickerStyle(.segmented)
                    .accessibilityIdentifier("parts")
                    .padding(.horizontal)
                    .padding(.vertical, 8)
                    switch part {
                    case .playlists: PlaylistsList(context: context)
                    case .songs: AllItemsList(context: context)
                    case .artists: NamesList(context: context, group: .artist)
                    case .albums: NamesList(context: context, group: .album)
                    }
                }
            }
        }
        .navigationTitle(tab)
        .searchable(text: $query, prompt: "Search \(noun)s, playlists, \(music ? "artists" : "channels")")
        .toolbar {
            if part == .playlists && !searching && newPlaylistTab != nil {
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        newName = ""
                        naming = true
                    } label: {
                        Label("New playlist", systemImage: "plus")
                    }
                    .accessibilityIdentifier("new-playlist")
                }
            }
        }
        .alert("New playlist", isPresented: $naming) {
            TextField("Name", text: $newName)
            Button("OK") {
                guard let id = newPlaylistTab else { return }
                let name = newName.trimmingCharacters(in: .whitespacesAndNewlines)
                edits.run { _ = try createPlaylist(tabId: id, name: name) }
            }
            Button("Cancel", role: .cancel) {}
        }
        // A tab saved to for the first time comes after its first save.
        .task(id: "\(tab)-\(retry)-\(jobs.ended)") {
            let tab = tab
            tabId = await Task.detached { () -> Loaded<Int64?> in
                do {
                    return .done(try tabIdOf(tab))
                } catch {
                    return .failed(describe(error))
                }
            }.value
        }
    }

    /// The tab a new playlist goes to, once it is known.
    private var newPlaylistTab: Int64? {
        if case .done(let id) = tabId { return id }
        return nil
    }

    private func label(_ part: LibraryPart) -> String {
        switch part {
        case .playlists: return "Playlists"
        case .songs: return music ? "Songs" : "Videos"
        case .artists: return music ? "Artists" : "Channels"
        case .albums: return "Albums"
        }
    }
}

/// What every part of a tab needs to know about it.
struct LibraryContext {
    let tab: String
    /// "song" or "video".
    let noun: String
    /// None when the tab is not there (nothing was ever saved to it).
    let tabId: Int64?

    var nouns: String { noun + "s" }
}

/// Something loaded off the main thread.
enum Loaded<T> {
    case loading
    case failed(String)
    case done(T)
}

/// The id of the library tab named `tab`, if there is one.
func tabIdOf(_ tab: String) throws -> Int64? {
    if let error = Core.startError { throw ShownError(text: "The core did not start: \(error)") }
    return try categories().first(where: { $0.name == tab && ($0.media == .audio || $0.media == .video) })?.id
}

struct ShownError: Error, CustomStringConvertible {
    let text: String
    var description: String { text }
}

/// The tab's playlists (smart ones have nothing in them yet, so they are not shown), each
/// with Rename, Copy and Delete.
private struct PlaylistsList: View {
    let context: LibraryContext
    @ObservedObject private var edits = LibraryEdits.shared
    @ObservedObject private var jobs = JobsStore.shared
    @State private var loaded: Loaded<[CollectionSummary]> = .loading
    /// What the alert asks about; it stays set while the alert goes away.
    @State private var ask: Ask?
    @State private var asking = false
    @State private var name = ""
    @State private var retry = 0

    var body: some View {
        List {
            switch loaded {
            case .loading:
                EmptyView()
            case .failed(let text):
                ProblemRow(text: text) { retry += 1 }
            case .done(let playlists) where playlists.isEmpty:
                Text("No playlists yet. Save one from the Add tab, or make your own.").foregroundStyle(.secondary)
            case .done(let playlists):
                ForEach(Array(playlists.enumerated()), id: \.element.id) { index, playlist in
                    NavigationLink {
                        PlaylistView(id: playlist.id, name: playlist.name, context: context)
                    } label: {
                        PlaylistLabel(playlist: playlist, context: context)
                    }
                    .accessibilityIdentifier("playlist-\(index)")
                    .contextMenu {
                        Button("Rename") { askAbout(.rename, playlist) }
                        Button("Copy") { askAbout(.copy, playlist) }
                        Button("Delete", role: .destructive) { askAbout(.delete, playlist) }
                    }
                    .swipeActions {
                        Button("Delete", role: .destructive) { askAbout(.delete, playlist) }
                    }
                }
            }
        }
        .listStyle(.plain)
        .refreshable { edits.refresh() }
        .task(id: "\(context.tabId ?? 0)-\(edits.count)-\(jobs.ended)-\(retry)") { await load() }
        .alert(Text(ask?.title ?? ""), isPresented: $asking, presenting: ask) { ask in
            switch ask.kind {
            case .rename, .copy:
                TextField("Name", text: $name)
                Button("OK") { answer(ask) }
                Button("Cancel", role: .cancel) {}
            case .delete:
                Button("Delete", role: .destructive) { answer(ask) }
                Button("Cancel", role: .cancel) {}
            }
        } message: { ask in
            switch ask.kind {
            case .rename: EmptyView()
            case .copy: Text("The copy is your own: it can be changed.")
            case .delete: Text("Its \(context.nouns) stay in your library.")
            }
        }
    }

    private struct Ask {
        enum Kind { case rename, copy, delete }
        let kind: Kind
        let playlist: CollectionSummary

        var title: String {
            switch kind {
            case .rename: return "Rename playlist"
            case .copy: return "Copy playlist"
            case .delete: return "Delete \u{201C}\(playlist.name)\u{201D}?"
            }
        }
    }

    private func askAbout(_ kind: Ask.Kind, _ playlist: CollectionSummary) {
        name = kind == .copy ? "\(playlist.name) (copy)" : playlist.name
        ask = Ask(kind: kind, playlist: playlist)
        asking = true
    }

    private func answer(_ ask: Ask) {
        let id = ask.playlist.id
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        switch ask.kind {
        case .rename: edits.run { try renamePlaylist(playlistId: id, name: name) }
        case .copy: edits.run { _ = try copyPlaylist(playlistId: id, name: name) }
        case .delete: edits.run { try deletePlaylist(playlistId: id) }
        }
    }

    private func load() async {
        guard let tabId = context.tabId else {
            loaded = .done([])
            return
        }
        loaded = await Task.detached { () -> Loaded<[CollectionSummary]> in
            do {
                return .done(try collections(categoryId: tabId).filter { !$0.smart })
            } catch {
                return .failed(describe(error))
            }
        }.value
    }
}

/// A playlist's name, how much is in it, and whether it follows a link.
struct PlaylistLabel: View {
    let playlist: CollectionSummary
    let context: LibraryContext

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(playlist.name).lineLimit(2)
            Text(countText(playlist.entries, context.noun) + (playlist.source != nil ? " · saved from a link" : ""))
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}

/// One playlist: what is in it, page by page; tap a row to play from there. One's own can
/// be changed (Edit to reorder, swipe or the menu to take out); a saved one follows its
/// link, and says so.
struct PlaylistView: View {
    let id: Int64
    let name: String
    let context: LibraryContext

    /// Entries asked for per page.
    private static let page: UInt32 = 100

    @ObservedObject private var playback = Playback.shared
    @ObservedObject private var jobs = JobsStore.shared
    @ObservedObject private var edits = LibraryEdits.shared
    @State private var summary: Loaded<CollectionSummary> = .loading
    @State private var rows: [CollectionEntry] = []
    @State private var next: String?
    @State private var complete = false
    @State private var loading = false
    @State private var failed: String?
    /// Bumped to start over (Retry).
    @State private var round = 0

    private var own: Bool {
        if case .done(let playlist) = summary { return playlist.source == nil }
        return false
    }

    var body: some View {
        List {
            switch summary {
            case .loading:
                EmptyView()
            case .failed(let text):
                ProblemRow(text: text) { round += 1 }
            case .done(let playlist):
                VStack(alignment: .leading, spacing: 6) {
                    HStack {
                        Text(countText(playlist.entries, context.noun))
                            .foregroundStyle(.secondary)
                            .accessibilityIdentifier("entry-count")
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
                    if playlist.source != nil {
                        Text("Saved from a link: saving it again updates it. Copy it to change it.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .accessibilityIdentifier("saved-note")
                    } else if playlist.entries == 0 {
                        Text("Nothing in it yet: add \(context.nouns) from their menu.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                ForEach(Array(rows.enumerated()), id: \.element.entryId) { index, row in
                    SongRow(index: index, row: row, playlist: id, context: context, own: own,
                            last: complete && index == rows.count - 1) { change in
                        switch change {
                        case .up: move(rows[index].entryId, to: index - 1)
                        case .down: move(rows[index].entryId, to: index + 2)
                        case .remove: take([rows[index].entryId])
                        }
                    }
                    .onAppear { if index == rows.count - 1 { Task { await more() } } }
                }
                .onMove(perform: own ? dragged : nil)
                .onDelete(perform: own ? swiped : nil)
                if let failed {
                    // The first page failed: start over; a later one: ask for it again.
                    ProblemRow(text: failed) { if next == nil { round += 1 } else { Task { await more() } } }
                }
            }
        }
        .navigationTitle(title)
        .toolbar {
            if own && !rows.isEmpty {
                ToolbarItem(placement: .primaryAction) { EditButton() }
            }
        }
        .safeAreaInset(edge: .top, spacing: 0) { ProblemBanner() }
        .task(id: "\(jobs.ended)-\(edits.count)-\(round)") { await reload() }
    }

    /// The newest name (it may have been renamed since the list was shown).
    private var title: String {
        if case .done(let playlist) = summary { return playlist.name }
        return name
    }

    /// SwiftUI says where a dragged row goes as the index before it leaves its place.
    private func dragged(from: IndexSet, to: Int) {
        guard let source = from.first, source < rows.count else { return }
        move(rows[source].entryId, to: to)
    }

    private func swiped(at offsets: IndexSet) {
        take(offsets.compactMap { $0 < rows.count ? rows[$0].entryId : nil })
    }

    /// Moves `entry` right before the row now at `place` (the end past the last one). Past
    /// the rows loaded so far, the one it goes before is asked for first.
    private func move(_ entry: Int64, to place: Int) {
        guard place >= 0 else { return }
        let id = id
        let shown = rows.map(\.entryId)
        let after = complete ? nil : next
        let more = !complete && place >= shown.count
        if place < shown.count && shown[place] == entry { return }
        edits.run {
            var before: Int64? = place < shown.count ? shown[place] : nil
            if more, let after {
                let ahead = place - shown.count
                let entries = try collectionItems(collectionId: id, after: after, limit: UInt32(ahead + 1)).entries
                before = ahead < entries.count ? entries[ahead].entryId : nil
            }
            if before == entry { return }
            try moveInPlaylist(playlistId: id, entry: entry, before: before)
        }
    }

    private func take(_ entries: [Int64]) {
        guard !entries.isEmpty else { return }
        let id = id
        edits.run { try removeFromPlaylist(playlistId: id, entries: entries) }
    }

    private func reload() async {
        let id = id
        let loaded = await Task.detached { () -> Loaded<CollectionSummary> in
            do {
                return .done(try collection(id: id))
            } catch {
                return .failed(describe(error))
            }
        }.value
        // Rows stay until the new ones come, so an edit does not flash the list empty.
        let first = await page(after: nil)
        summary = loaded
        switch first {
        case .success(let page):
            rows = page.entries
            next = page.next
            complete = page.next == nil
            failed = nil
        case .failure(let error):
            rows = []
            next = nil
            complete = false
            failed = describe(error)
        }
    }

    private func more() async {
        guard !loading, !complete, let after = next, case .done = summary else { return }
        loading = true
        defer { loading = false }
        switch await page(after: after) {
        case .success(let page):
            // A reload may have started over meanwhile.
            guard next == after else { return }
            rows += page.entries
            next = page.next
            complete = page.next == nil
            failed = nil
        case .failure(let error):
            failed = describe(error)
        }
    }

    private func page(after: String?) async -> Result<CollectionPage, Error> {
        let id = id
        return await Task.detached {
            Result { try collectionItems(collectionId: id, after: after, limit: PlaylistView.page) }
        }.value
    }
}

/// What a playlist row's menu asks for.
enum EntryChange {
    case up, down, remove
}

/// A song of a playlist: bold while it plays, dimmed when its file is gone.
private struct SongRow: View {
    let index: Int
    let row: CollectionEntry
    let playlist: Int64
    let context: LibraryContext
    let own: Bool
    /// The playlist's last entry (nothing after it, loaded or not).
    let last: Bool
    let change: (EntryChange) -> Void
    @ObservedObject private var playback = Playback.shared
    @State private var picking = false

    var body: some View {
        let item = row.item
        let here = playback.source == playlist && playback.state?.nowPlaying?.itemId == item.id
        let skipped = playback.source == playlist && (playback.state?.skipped.contains { $0.itemId == item.id } ?? false)
        let note: String = {
            if item.missing { return "File missing" }
            if skipped { return "Could not play" }
            return [item.artist, item.durationMs.map(clock)].compactMap { $0 }.joined(separator: " · ")
        }()
        Button {
            if !item.missing { playback.playCollection(id: playlist, startEntry: row.entryId) }
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
                Button("Add to playlist…") { picking = true }
            }
            if own {
                if index > 0 { Button("Move up") { change(.up) } }
                if !last { Button("Move down") { change(.down) } }
                Button("Remove from playlist", role: .destructive) { change(.remove) }
            }
        }
        .opacity(item.missing ? 0.4 : 1)
        .sheet(isPresented: $picking) { PlaylistPicker(context: context, items: [item.id]) }
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

func countText(_ count: Int64, _ noun: String) -> String {
    count == 1 ? "1 \(noun)" : "\(count) \(noun)s"
}

/// 3:07, or 1:02:03 from an hour.
func clock(_ ms: Int64) -> String {
    let seconds = ms / 1000
    let (hours, minutes, rest) = (seconds / 3600, seconds / 60 % 60, seconds % 60)
    return hours > 0 ? String(format: "%lld:%02lld:%02lld", hours, minutes, rest) : String(format: "%lld:%02lld", minutes, rest)
}
