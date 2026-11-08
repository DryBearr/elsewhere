import SwiftUI

/// Every job, newest first. Tapping one opens its log.
struct DownloadsView: View {
    @ObservedObject private var store = JobsStore.shared

    var body: some View {
        List(store.newestFirst, id: \.id) { job in
            NavigationLink(value: job.id) { JobRow(job: job) }
        }
        .overlay {
            if store.jobs.isEmpty {
                Text("No downloads yet.").foregroundStyle(.secondary)
            }
        }
        .navigationTitle("Downloads")
        .navigationDestination(for: Int64.self) { id in JobDetailView(id: id) }
    }
}

/// One job: title, status, progress, the current item, the last error and its buttons.
struct JobRow: View {
    let job: JobState
    private var store: JobsStore { JobsStore.shared }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(job.title).font(.headline).lineLimit(1)
                Spacer()
                Text(label(job.status)).font(.caption).foregroundStyle(.secondary)
            }
            let finished = Int(job.done + job.failed)
            if let total = job.total, total > 0 {
                ProgressView(value: Double(finished), total: Double(total))
            }
            Text("\(finished) of \(job.total.map { "\($0)" } ?? "?")" + (job.failed > 0 ? " · \(job.failed) failed" : ""))
                .font(.caption)
            if let item = job.current {
                Text("\(item.position). \(item.title)").font(.caption).lineLimit(1)
                if let size = item.totalBytes, size > 0 {
                    ProgressView(value: Double(item.bytes), total: Double(size))
                }
                Text(transfer(item)).font(.caption2).foregroundStyle(.secondary)
            }
            if let error = job.lastError {
                Text(error).font(.caption).foregroundStyle(.red)
            }
            HStack(spacing: 16) {
                switch job.status {
                case .queued, .running:
                    Button("Cancel") { store.cancel(job.id) }
                case .interrupted:
                    Button("Resume") { store.resume(job.id) }
                    Button("Discard", role: .destructive) { store.discard(job.id) }
                case .failed:
                    Button("Retry") { store.resume(job.id) }
                    Button("Remove", role: .destructive) { store.forget(job.id) }
                case .done, .cancelled:
                    Button("Remove", role: .destructive) { store.forget(job.id) }
                }
            }
            // Buttons in a list row each act on their own, not on the whole row.
            .buttonStyle(.borderless)
            .font(.callout)
        }
        .padding(.vertical, 4)
    }
}

/// One job with its whole log (info, warnings, errors), which grows while it runs.
struct JobDetailView: View {
    let id: Int64
    @ObservedObject private var store = JobsStore.shared
    @State private var lines: [JobLogLine] = []

    var body: some View {
        List {
            if let job = store.jobs[id] {
                JobRow(job: job)
                Section("Log") {
                    ForEach(lines, id: \.number) { line in
                        Text("\(line.number). \(line.message)")
                            .font(.caption)
                            .foregroundStyle(color(line.level))
                            .textSelection(.enabled)
                    }
                }
            } else {
                Text("This job was removed.")
            }
        }
        .navigationTitle("Job")
        .task(id: store.jobs[id]?.logLines) {
            let since = lines.last?.number ?? 0
            let new = await Task.detached { (try? jobLogs(id: id, since: since)) ?? [] }.value
            lines.append(contentsOf: new)
        }
    }
}

private func label(_ status: JobStatus) -> String {
    switch status {
    case .queued: return "Waiting"
    case .running: return "Running"
    case .interrupted: return "Interrupted"
    case .done: return "Done"
    case .failed: return "Failed"
    case .cancelled: return "Cancelled"
    }
}

private func color(_ level: LogLevel) -> Color {
    switch level {
    case .error: return .red
    case .warning: return .orange
    default: return .primary
    }
}

private func transfer(_ item: JobItem) -> String {
    let format = ByteCountFormatter()
    var text = format.string(fromByteCount: item.bytes)
    if let total = item.totalBytes { text += " of " + format.string(fromByteCount: total) }
    if let speed = item.speed { text += " · " + format.string(fromByteCount: Int64(speed)) + "/s" }
    if let eta = item.eta { text += String(format: " · %d:%02d left", eta / 60, eta % 60) }
    return text
}
