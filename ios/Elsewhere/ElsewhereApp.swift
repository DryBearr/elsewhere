import SwiftUI

/// The app's tabs. Each screen is its own file; list it here to plug it in.
enum Tab: Hashable {
    case music, videos, downloads, add, library
}

@main
struct ElsewhereApp: App {
    @State private var tab = Tab.downloads

    init() {
        Core.boot()
        Playback.shared.start()
        SelfTest.run(arguments: ProcessInfo.processInfo.arguments)
    }

    var body: some Scene {
        WindowGroup {
            TabView(selection: $tab) {
                NavigationStack { MusicView() }
                    .playerBar()
                    .tabItem { Label("Music", systemImage: "music.note.list") }
                    .tag(Tab.music)
                NavigationStack { MusicView(tab: "Videos") }
                    .playerBar()
                    .tabItem { Label("Videos", systemImage: "film") }
                    .tag(Tab.videos)
                NavigationStack { DownloadsView() }
                    .playerBar()
                    .tabItem { Label("Downloads", systemImage: "arrow.down.circle") }
                    .tag(Tab.downloads)
                NavigationStack { AddView { tab = .downloads } }
                    .playerBar()
                    .tabItem { Label("Add", systemImage: "plus") }
                    .tag(Tab.add)
                NavigationStack { LibraryView() }
                    .playerBar()
                    .tabItem { Label("Library", systemImage: "books.vertical") }
                    .tag(Tab.library)
            }
            .playerSheet()
        }
    }
}

/// The text of an error from the Rust core (or any other error).
func describe(_ error: Error) -> String {
    if case let ApiError.Failed(problem) = error {
        return problem.text
    }
    return "\(error)"
}
