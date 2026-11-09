import SwiftUI

/// Speeds the speed menu offers.
private let speeds: [Double] = [0.5, 0.75, 1, 1.25, 1.5, 2]

/// Queue rows read from the library at once.
private let chunk = 100

/// The player, as a sheet over the tabs: the cover, what plays, a position slider, the
/// controls (with repeat, shuffle and speed) and the queue (tap to play, swipe to remove,
/// Edit to reorder, hold for Play next). Swipe down or Close closes it; it closes itself
/// when the queue empties. Playback goes on either way. Plugged in by `.playerSheet()`.
struct PlayerView: View {
    @ObservedObject private var playback = Playback.shared
    @Environment(\.dismiss) private var dismiss
    /// Rows by item id (nil: no longer in the library), read as they come into view.
    @State private var rows: [Int64: ItemSummary?] = [:]
    @State private var loading: Set<Int> = []

    var body: some View {
        NavigationStack {
            if let queue = playback.state, let current = queue.current, current < queue.entryCount {
                let listed = playback.entries?.revision == queue.entriesRevision ? playback.entries?.entries ?? [] : []
                List {
                    Section {
                        PlayerTop(queue: queue)
                            .listRowSeparator(.hidden)
                    }
                    Section("Queue · \(queue.entryCount)") {
                        ForEach(Array(listed.enumerated()), id: \.element.key) { index, entry in
                            row(index: index, entry: entry, playing: entry.key == queue.currentKey, listed: listed)
                        }
                        .onMove { from, to in
                            guard let first = from.first else { return }
                            // SwiftUI counts `to` before the entry leaves its place.
                            playback.moveEntry(key: listed[first].key, to: to > first ? to - 1 : to)
                        }
                        .onDelete { offsets in
                            for offset in offsets { playback.remove(key: listed[offset].key) }
                        }
                    }
                }
                .listStyle(.plain)
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Close") { dismiss() }.accessibilityIdentifier("close-player")
                    }
                    ToolbarItem(placement: .primaryAction) { EditButton() }
                }
            } else {
                // The queue emptied (stopped, last entry removed): nothing to show.
                Color.clear.onAppear { dismiss() }
            }
        }
        .onAppear { playback.wantEntries(true) }
        .onDisappear { playback.wantEntries(false) }
    }

    private func row(index: Int, entry: QueueEntry, playing: Bool, listed: [QueueEntry]) -> some View {
        let known = rows.keys.contains(entry.itemId)
        let summary = rows[entry.itemId] ?? nil
        return Button {
            playback.jump(key: entry.key)
        } label: {
            VStack(alignment: .leading, spacing: 2) {
                Text(summary?.title ?? (known ? "Not in the library any more" : " "))
                    .lineLimit(1)
                    .fontWeight(playing ? .bold : .regular)
                    .foregroundStyle(playing ? Color.accentColor : Color.primary)
                if let artist = summary?.artist {
                    Text(artist).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("queue-\(index)")
        .contextMenu {
            if !playing {
                Button("Play next") { playback.playNext(key: entry.key) }
            }
            Button("Remove", role: .destructive) { playback.remove(key: entry.key) }
        }
        .onAppear { load(around: index, in: listed) }
    }

    /// Reads the rows of the chunk around `index` once.
    private func load(around index: Int, in listed: [QueueEntry]) {
        let start = index / chunk * chunk
        let ids = Array(Set(listed[start..<min(listed.count, start + chunk)].map(\.itemId)))
            .filter { !rows.keys.contains($0) }
        guard !ids.isEmpty, !loading.contains(start) else { return }
        loading.insert(start)
        Task {
            let found = await Task.detached { try? items(ids: ids) }.value
            loading.remove(start)
            guard let found else { return }
            for (id, row) in zip(ids, found) { rows[id] = .some(row) }
        }
    }
}

/// The top of the player: cover, titles, position, controls.
private struct PlayerTop: View {
    let queue: QueueState
    @ObservedObject private var playback = Playback.shared
    @State private var cover: UIImage?
    @State private var coverPath: String?
    @State private var playlist: String?

    var body: some View {
        let now = queue.nowPlaying
        let failed = queue.finished && queue.playback.status == .failed
        let below: String? = {
            if failed { return queue.problem?.text ?? "This could not be played." }
            if let message = playback.message { return message }
            let parts = [now?.artist, now?.album].compactMap { $0 }
            return parts.isEmpty ? nil : parts.joined(separator: " · ")
        }()
        VStack(spacing: 10) {
            if let playlist {
                Text(playlist).font(.subheadline).foregroundStyle(.secondary).lineLimit(1)
            }
            ZStack {
                RoundedRectangle(cornerRadius: 12).fill(Color.secondary.opacity(0.15))
                if let cover {
                    Image(uiImage: cover).resizable().scaledToFill()
                        .accessibilityLabel("Cover")
                } else {
                    Image(systemName: "music.note").font(.system(size: 72)).foregroundStyle(.secondary)
                }
            }
            .aspectRatio(1, contentMode: .fit)
            .frame(maxWidth: 360)
            .clipShape(RoundedRectangle(cornerRadius: 12))
            Text(now?.title ?? "Opening…")
                .font(.title2.weight(.semibold))
                .multilineTextAlignment(.center)
                .lineLimit(2)
                .accessibilityIdentifier("player-title")
            if let below {
                Text(below)
                    .font(.subheadline)
                    .foregroundStyle(failed || playback.message != nil ? Color.red : Color.secondary)
                    .multilineTextAlignment(.center)
                    .lineLimit(2)
                    .accessibilityIdentifier("player-below")
            }
            Position(queue: queue)
            Controls(queue: queue)
        }
        .frame(maxWidth: .infinity)
        .task(id: now?.cover) { await loadCover(now?.cover) }
        .task(id: playback.source) {
            let id = playback.source
            playlist = await Task.detached { id.flatMap { try? collection(id: $0).name } }.value
        }
    }

    private func loadCover(_ path: String?) async {
        guard path != coverPath else { return }
        coverPath = path
        cover = nil
        guard let path else { return }
        cover = await Task.detached { Playback.loadCover(path, size: 1024) }.value
    }
}

/// The position slider and the times; while dragging, the slider follows the finger only.
private struct Position: View {
    let queue: QueueState
    @ObservedObject private var playback = Playback.shared
    @State private var dragging: Double?
    /// After a seek, its spot shows until the player gets there (or 3 s pass).
    @State private var sought: (target: Int64, at: Date)?

    var body: some View {
        let state = queue.playback
        let duration = state.durationMs ?? 0
        let position = queue.finished ? duration : state.positionMs
        let pending = sought.flatMap { sought in
            abs(position - sought.target) > 1500 && Date().timeIntervalSince(sought.at) < 3 ? sought.target : nil
        }
        let shown = dragging.map { Int64($0) } ?? pending ?? position
        VStack(spacing: 2) {
            if duration > 0 {
                Slider(
                    value: Binding(
                        get: { Double(min(max(0, shown), duration)) },
                        set: { dragging = $0 }
                    ),
                    in: 0...Double(duration),
                    onEditingChanged: { editing in
                        guard !editing, let target = dragging else { return }
                        sought = (Int64(target), Date())
                        playback.seek(positionMs: Int64(target))
                        dragging = nil
                    }
                )
                .accessibilityIdentifier("player-position")
            }
            HStack {
                Text(time(shown)).accessibilityIdentifier("player-elapsed")
                Spacer()
                if duration > 0 { Text("-" + time(duration - min(max(0, shown), duration))) }
            }
            .font(.caption.monospacedDigit())
            .foregroundStyle(.secondary)
        }
    }
}

private struct Controls: View {
    let queue: QueueState
    @ObservedObject private var playback = Playback.shared

    var body: some View {
        VStack(spacing: 8) {
            HStack {
                Button { playback.setShuffle(!queue.shuffle) } label: {
                    Image(systemName: "shuffle").foregroundStyle(queue.shuffle ? Color.accentColor : Color.primary)
                }
                .accessibilityLabel(queue.shuffle ? "Shuffle on" : "Shuffle off")
                .accessibilityIdentifier("shuffle")
                Spacer()
                Button { playback.previous() } label: { Image(systemName: "backward.fill") }
                    .accessibilityLabel("Previous")
                    .accessibilityIdentifier("player-previous")
                Spacer()
                if queue.wantsPlay {
                    Button { playback.pause() } label: { Image(systemName: "pause.circle.fill").font(.system(size: 56)) }
                        .accessibilityLabel("Pause")
                        .accessibilityIdentifier("player-pause")
                } else {
                    Button { playback.resume() } label: { Image(systemName: "play.circle.fill").font(.system(size: 56)) }
                        .accessibilityLabel("Play")
                        .accessibilityIdentifier("player-play")
                }
                Spacer()
                Button { playback.next() } label: { Image(systemName: "forward.fill") }
                    .accessibilityLabel("Next")
                    .accessibilityIdentifier("player-next")
                    .disabled(!queue.hasNext)
                Spacer()
                Button {
                    playback.setRepeat({
                        switch queue.repeat {
                        case .off: return .all
                        case .all: return .one
                        case .one: return .off
                        }
                    }())
                } label: {
                    Image(systemName: queue.repeat == .one ? "repeat.1" : "repeat")
                        .foregroundStyle(queue.repeat == .off ? Color.primary : Color.accentColor)
                }
                .accessibilityLabel({
                    switch queue.repeat {
                    case .off: return "Repeat off"
                    case .all: return "Repeat all"
                    case .one: return "Repeat one"
                    }
                }())
                .accessibilityIdentifier("repeat")
            }
            .font(.title2)
            .buttonStyle(.plain)
            Menu {
                ForEach(speeds, id: \.self) { speed in
                    Button(speedText(speed)) { playback.setSpeed(speed) }
                }
            } label: {
                Text("Speed \(speedText(queue.playback.speed))").font(.subheadline)
            }
            .accessibilityIdentifier("speed")
        }
        .padding(.vertical, 6)
    }
}

/// 1x, 0.75x, 1.5x.
private func speedText(_ speed: Double) -> String {
    (speed == speed.rounded() ? String(Int(speed)) : String(speed)) + "x"
}

/// 3:07, or 1:02:03 from an hour.
private func time(_ ms: Int64) -> String {
    let seconds = max(0, ms) / 1000
    let (hours, minutes, rest) = (seconds / 3600, seconds / 60 % 60, seconds % 60)
    return hours > 0 ? String(format: "%lld:%02lld:%02lld", hours, minutes, rest) : String(format: "%lld:%02lld", minutes, rest)
}

extension View {
    /// The player sheet, opened by the player bar.
    func playerSheet() -> some View {
        modifier(PlayerSheet())
    }
}

private struct PlayerSheet: ViewModifier {
    @ObservedObject private var playback = Playback.shared

    func body(content: Content) -> some View {
        content.sheet(isPresented: $playback.playerOpen) { PlayerView() }
    }
}
