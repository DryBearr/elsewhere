import SwiftUI

/// The small player above the tabs while something is queued: what plays, its progress,
/// previous, play/pause, next and stop. Plugged in by `.playerBar()` on each tab in
/// ElsewhereApp.
struct PlayerBar: View {
    @ObservedObject private var playback = Playback.shared

    var body: some View {
        if let queue = playback.state, let index = queue.current, Int(index) < queue.entries.count {
            bar(queue)
        } else if let message = playback.message {
            // Nothing queued: only a refusal shows (for example no sound during a call).
            Text(message)
                .font(.caption)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(12)
                .background(.bar)
        }
    }

    private func bar(_ queue: QueueState) -> some View {
        let playback = queue.playback
        let failed = queue.finished && playback.status == .failed
        let below: String? = {
            if failed { return queue.problem?.text ?? "This could not be played." }
            if let message = self.playback.message { return message }
            if queue.finished { return "Finished" }
            return queue.nowPlaying?.artist
        }()
        return VStack(spacing: 0) {
            if let duration = playback.durationMs, duration > 0 {
                ProgressView(value: queue.finished ? 1 : min(1, max(0, Double(playback.positionMs) / Double(duration))))
                    .accessibilityLabel("Progress")
            }
            HStack(spacing: 16) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(queue.nowPlaying?.title ?? "Opening…")
                        .lineLimit(1)
                        .accessibilityIdentifier("now-title")
                    if let below {
                        Text(below)
                            .font(.caption)
                            .lineLimit(1)
                            .foregroundStyle(failed || self.playback.message != nil ? Color.red : Color.secondary)
                            .accessibilityIdentifier("now-below")
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                Button { self.playback.previous() } label: {
                    Image(systemName: "backward.fill")
                }
                .accessibilityLabel("Previous")
                .accessibilityIdentifier("bar-previous")
                if queue.wantsPlay {
                    Button { self.playback.pause() } label: { Image(systemName: "pause.fill") }
                        .accessibilityLabel("Pause")
                        .accessibilityIdentifier("bar-pause")
                } else {
                    Button { self.playback.resume() } label: { Image(systemName: "play.fill") }
                        .accessibilityLabel("Play")
                        .accessibilityIdentifier("bar-play")
                }
                Button { self.playback.next() } label: {
                    Image(systemName: "forward.fill")
                }
                .accessibilityLabel("Next")
                .accessibilityIdentifier("bar-next")
                .disabled(!queue.hasNext)
                Button { self.playback.stop() } label: {
                    Image(systemName: "xmark")
                }
                .accessibilityLabel("Stop")
                .accessibilityIdentifier("bar-stop")
            }
            .font(.title3)
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
        }
        .background(.bar)
    }
}

extension View {
    /// The player bar at the bottom of a tab, above the tab bar.
    func playerBar() -> some View {
        safeAreaInset(edge: .bottom, spacing: 0) { PlayerBar() }
    }
}
