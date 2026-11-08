import SwiftUI

/// The app's one screen. Each feature is a file with one view; list it here to plug it in.
@main
struct ElsewhereApp: App {
    var body: some Scene {
        WindowGroup {
            NavigationStack {
                List {
                    YtdlpView()
                    CategoriesView()
                }
                .navigationTitle("Elsewhere")
            }
        }
    }
}

/// The text of an error from the Rust core (or any other error).
func describe(_ error: Error) -> String {
    if let error = error as? ApiError {
        switch error {
        case let .NotReady(message), let .BadArgs(message), let .Cancelled(message),
             let .YtDlp(message), let .Failed(message):
            return message
        }
    }
    return "\(error)"
}

/// The app's folders, for the Rust core.
enum AppFolders {
    static var data: URL {
        let url = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    static var cache: URL {
        FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
    }
}
