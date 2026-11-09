import SwiftUI
import UIKit

/// Changes to the library made from these screens: they run one after the other off the
/// main thread, and every screen showing the library looks again after each one. Call it
/// on the main thread.
final class LibraryEdits: ObservableObject {
    static let shared = LibraryEdits()

    /// Goes up after every change (and on pull to refresh).
    @Published private(set) var count = 0
    /// Why the last change did not happen; nil after one works.
    @Published var problem: String?

    /// One after the other, so a move and then a removal reach the core in that order.
    private let queue = DispatchQueue(label: "elsewhere-library-edits")

    private init() {}

    func run(_ change: @escaping () throws -> Void) {
        queue.async {
            var problem: String?
            do {
                try change()
            } catch {
                problem = describe(error)
            }
            DispatchQueue.main.async {
                self.problem = problem
                self.count += 1
            }
        }
    }

    func refresh() {
        count += 1
    }
}

extension JobsStore {
    /// Jobs that ended: saving a playlist changes the library, so screens look again.
    var ended: Int { jobs.values.filter { $0.status.isFinished }.count }
}

/// Why the last change did not happen, until it is tapped away.
struct ProblemBanner: View {
    @ObservedObject private var edits = LibraryEdits.shared

    var body: some View {
        if let problem = edits.problem {
            Button {
                edits.problem = nil
            } label: {
                Label(problem, systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.red)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal)
                    .padding(.vertical, 8)
                    .background(.bar)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("edit-problem")
        }
    }
}

/// Everything the tab holds: A to Z, newest first, or last played first.
struct AllItemsList: View {
    let context: LibraryContext
    @State private var order = ItemOrder.title

    var body: some View {
        if let tabId = context.tabId {
            PagedItems(
                context: context,
                key: "all-\(order)",
                empty: "Nothing here yet. Save something from the Add tab.",
                play: false,
                header: {
                    Picker("Order", selection: $order) {
                        Text("A to Z").tag(ItemOrder.title)
                        Text("Newest").tag(ItemOrder.added)
                        Text("Played").tag(ItemOrder.played)
                    }
                    .pickerStyle(.segmented)
                    .accessibilityIdentifier("order")
                }
            ) { [order = order] after in
                try tabItems(tabId: tabId, order: order, after: after, limit: browsePage)
            }
        } else {
            List { Text("Nothing here yet. Save something from the Add tab.").foregroundStyle(.secondary) }
                .listStyle(.plain)
        }
    }
}

/// Items asked for per page while browsing.
private let browsePage: UInt32 = 100

/// Artists (channels for videos) or albums.
enum NameGroup {
    case artist, album
}

/// The tab's artists or albums, A to Z; open one to see its items.
struct NamesList: View {
    let context: LibraryContext
    let group: NameGroup
    @ObservedObject private var edits = LibraryEdits.shared
    @ObservedObject private var jobs = JobsStore.shared
    @State private var loaded: Loaded<[Named]> = .loading
    @State private var retry = 0

    var body: some View {
        List {
            switch loaded {
            case .loading:
                EmptyView()
            case .failed(let text):
                ProblemRow(text: text) { retry += 1 }
            case .done(let names) where names.isEmpty:
                Text(group == .album ? "No albums yet." : context.noun == "song" ? "No artists yet." : "No channels yet.")
                    .foregroundStyle(.secondary)
            case .done(let names):
                ForEach(Array(names.enumerated()), id: \.element.name) { index, named in
                    NavigationLink {
                        NamedItemsView(context: context, group: group, name: named.name)
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(named.name).lineLimit(2)
                            Text(countText(named.items, context.noun)).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    .accessibilityIdentifier("name-\(index)")
                }
            }
        }
        .listStyle(.plain)
        .refreshable { edits.refresh() }
        .task(id: "\(group)-\(context.tabId ?? 0)-\(edits.count)-\(jobs.ended)-\(retry)") {
            guard let tabId = context.tabId else {
                loaded = .done([])
                return
            }
            let group = group
            loaded = await Task.detached { () -> Loaded<[Named]> in
                do {
                    return .done(try group == .artist ? artists(tabId: tabId) : albums(tabId: tabId))
                } catch {
                    return .failed(describe(error))
                }
            }.value
        }
    }
}

/// An artist's or album's items, with Play.
struct NamedItemsView: View {
    let context: LibraryContext
    let group: NameGroup
    let name: String

    var body: some View {
        Group {
            if let tabId = context.tabId {
                PagedItems(context: context, key: "\(group)-\(name)", empty: "Nothing here any more.", play: true) { [group = group, name = name] after in
                    if group == .artist {
                        return try artistItems(tabId: tabId, artist: name, after: after, limit: browsePage)
                    }
                    return try albumItems(tabId: tabId, album: name, after: after, limit: browsePage)
                }
            } else {
                List { Text("Nothing here any more.").foregroundStyle(.secondary) }
            }
        }
        .navigationTitle(name)
        .safeAreaInset(edge: .top, spacing: 0) { ProblemBanner() }
    }
}

/// Items a page at a time, from `load` (given the `next` of the page before); tap one to
/// play the ones loaded from there. With `play`, a Play button plays them from the top.
/// It starts over when `key` changes or the library does.
private struct PagedItems<Header: View>: View {
    let context: LibraryContext
    let key: String
    let empty: String
    let play: Bool
    var header: () -> Header
    let load: (String?) throws -> ItemPage

    @ObservedObject private var playback = Playback.shared
    @ObservedObject private var edits = LibraryEdits.shared
    @ObservedObject private var jobs = JobsStore.shared
    @State private var rows: [ItemSummary] = []
    @State private var next: String?
    @State private var complete = false
    @State private var loading = false
    @State private var loadedOnce = false
    @State private var failed: String?
    @State private var retry = 0

    init(context: LibraryContext, key: String, empty: String, play: Bool,
         @ViewBuilder header: @escaping () -> Header, load: @escaping (String?) throws -> ItemPage) {
        self.context = context
        self.key = key
        self.empty = empty
        self.play = play
        self.header = header
        self.load = load
    }

    var body: some View {
        List {
            header()
            if play && !rows.isEmpty {
                HStack {
                    Text(complete ? countText(Int64(rows.count), context.noun) : "")
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button {
                        playback.play(items: rows.map(\.id), start: 0)
                    } label: {
                        Label("Play", systemImage: "play.fill")
                    }
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier("play-all")
                }
            }
            if loadedOnce && rows.isEmpty && failed == nil {
                Text(empty).foregroundStyle(.secondary)
            }
            ForEach(Array(rows.enumerated()), id: \.offset) { index, item in
                ItemRow(identifier: "item-\(index)", item: item, context: context) {
                    playback.play(items: rows.map(\.id), start: index)
                }
                .onAppear { if index == rows.count - 1 { Task { await more() } } }
            }
            if let failed {
                // The first page failed: start over; a later one: ask for it again.
                ProblemRow(text: failed) { if next == nil { retry += 1 } else { Task { await more() } } }
            }
        }
        .listStyle(.plain)
        .refreshable { edits.refresh() }
        .task(id: "\(key)-\(edits.count)-\(jobs.ended)-\(retry)") { await reload() }
    }

    private func reload() async {
        let first = await page(after: nil)
        loadedOnce = true
        switch first {
        case .success(let page):
            rows = page.items
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
        guard !loading, !complete, let after = next else { return }
        loading = true
        defer { loading = false }
        switch await page(after: after) {
        case .success(let page):
            // A reload may have started over meanwhile.
            guard next == after else { return }
            rows += page.items
            next = page.next
            complete = page.next == nil
            failed = nil
        case .failure(let error):
            failed = describe(error)
        }
    }

    private func page(after: String?) async -> Result<ItemPage, Error> {
        let load = load
        return await Task.detached { Result { try load(after) } }.value
    }
}

extension PagedItems where Header == EmptyView {
    init(context: LibraryContext, key: String, empty: String, play: Bool, load: @escaping (String?) throws -> ItemPage) {
        self.init(context: context, key: key, empty: empty, play: play, header: { EmptyView() }, load: load)
    }
}

/// An item: tap to play (`play`); its menu queues it or adds it to one of one's playlists.
private struct ItemRow: View {
    let identifier: String
    let item: ItemSummary
    let context: LibraryContext
    let play: () -> Void
    @ObservedObject private var playback = Playback.shared
    @State private var picking = false

    var body: some View {
        let here = playback.state?.nowPlaying?.itemId == item.id
        let note = item.missing
            ? "File missing"
            : [item.artist, item.durationMs.map(clock)].compactMap { $0 }.joined(separator: " · ")
        Button {
            if !item.missing { play() }
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
        .accessibilityIdentifier(identifier)
        .contextMenu {
            if !item.missing {
                Button("Play next") { playback.addNext(itemId: item.id) }
                Button("Add to queue") { playback.add(itemId: item.id) }
                Button("Add to playlist…") { picking = true }
            }
        }
        .opacity(item.missing ? 0.4 : 1)
        .sheet(isPresented: $picking) { PlaylistPicker(context: context, items: [item.id]) }
    }
}

/// What a search finds as one types (once typing pauses): items, playlists, artists.
struct SearchList: View {
    let context: LibraryContext
    let query: String
    @ObservedObject private var playback = Playback.shared
    @ObservedObject private var edits = LibraryEdits.shared
    @ObservedObject private var jobs = JobsStore.shared
    @State private var found: Loaded<SearchResult> = .loading

    var body: some View {
        List {
            switch found {
            case .loading:
                EmptyView()
            case .failed(let text):
                Text(text).foregroundStyle(.red)
            case .done(let result) where result.items.isEmpty && result.playlists.isEmpty && result.artists.isEmpty:
                Text("Nothing found.").foregroundStyle(.secondary)
            case .done(let result):
                if !result.items.isEmpty {
                    Section(context.nouns.capitalized) {
                        ForEach(Array(result.items.enumerated()), id: \.offset) { index, item in
                            ItemRow(identifier: "found-\(index)", item: item, context: context) {
                                playback.play(items: result.items.map(\.id), start: index)
                                // The keyboard goes, so the player bar shows; the search stays.
                                UIApplication.shared.sendAction(
                                    #selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
                            }
                        }
                    }
                }
                if !result.playlists.isEmpty {
                    Section("Playlists") {
                        ForEach(Array(result.playlists.enumerated()), id: \.element.id) { index, playlist in
                            NavigationLink {
                                PlaylistView(id: playlist.id, name: playlist.name, context: context)
                            } label: {
                                PlaylistLabel(playlist: playlist, context: context)
                            }
                            .accessibilityIdentifier("found-playlist-\(index)")
                        }
                    }
                }
                if !result.artists.isEmpty {
                    Section(context.noun == "song" ? "Artists" : "Channels") {
                        ForEach(Array(result.artists.enumerated()), id: \.element.name) { index, named in
                            NavigationLink {
                                NamedItemsView(context: context, group: .artist, name: named.name)
                            } label: {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(named.name).lineLimit(2)
                                    Text(countText(named.items, context.noun)).font(.caption).foregroundStyle(.secondary)
                                }
                            }
                            .accessibilityIdentifier("found-artist-\(index)")
                        }
                    }
                }
            }
        }
        .listStyle(.plain)
        .scrollDismissesKeyboard(.immediately)
        .task(id: "\(query)-\(context.tabId ?? 0)-\(edits.count)-\(jobs.ended)") {
            // Typing: look once it pauses.
            try? await Task.sleep(nanoseconds: 250_000_000)
            if Task.isCancelled { return }
            guard let tabId = context.tabId else {
                found = .done(SearchResult(items: [], playlists: [], artists: []))
                return
            }
            let query = query
            let result = await Task.detached { () -> Loaded<SearchResult> in
                do {
                    return .done(try search(tabId: tabId, query: query))
                } catch {
                    return .failed(describe(error))
                }
            }.value
            if !Task.isCancelled { found = result }
        }
    }
}

/// Picks one of one's own playlists of the tab (or makes a new one) and adds `items` to it.
struct PlaylistPicker: View {
    let context: LibraryContext
    let items: [Int64]
    @Environment(\.dismiss) private var dismiss
    @ObservedObject private var edits = LibraryEdits.shared
    @State private var own: Loaded<[CollectionSummary]> = .loading
    @State private var naming = false
    @State private var name = ""

    var body: some View {
        NavigationStack {
            List {
                Button {
                    name = ""
                    naming = true
                } label: {
                    Label("New playlist…", systemImage: "plus")
                }
                .accessibilityIdentifier("pick-new")
                switch own {
                case .loading:
                    EmptyView()
                case .failed(let text):
                    Text(text).foregroundStyle(.red)
                case .done(let playlists):
                    ForEach(Array(playlists.enumerated()), id: \.element.id) { index, playlist in
                        Button {
                            let id = playlist.id
                            let items = items
                            edits.run { _ = try addToPlaylist(playlistId: id, items: items, before: nil) }
                            dismiss()
                        } label: {
                            PlaylistLabel(playlist: playlist, context: context)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityIdentifier("pick-\(index)")
                    }
                }
            }
            .navigationTitle("Add to playlist")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
            .alert("New playlist", isPresented: $naming) {
                TextField("Name", text: $name)
                Button("OK") {
                    guard let tabId = context.tabId else { return }
                    let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
                    let items = items
                    edits.run {
                        let made = try createPlaylist(tabId: tabId, name: name)
                        _ = try addToPlaylist(playlistId: made, items: items, before: nil)
                    }
                    dismiss()
                }
                Button("Cancel", role: .cancel) {}
            }
            .task {
                guard let tabId = context.tabId else {
                    own = .failed("This tab is not in the library yet.")
                    return
                }
                own = await Task.detached { () -> Loaded<[CollectionSummary]> in
                    do {
                        return .done(try collections(categoryId: tabId).filter { $0.source == nil && !$0.smart })
                    } catch {
                        return .failed(describe(error))
                    }
                }.value
            }
        }
        .presentationDetents([.medium, .large])
    }
}
