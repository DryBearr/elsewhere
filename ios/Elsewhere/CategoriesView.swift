import SwiftUI

/// The library's categories, from the database in the Rust core.
struct CategoriesView: View {
    @State private var names: [String] = []

    var body: some View {
        Section("Categories") {
            ForEach(names, id: \.self) { Text($0) }
        }
        .task {
            let path = AppFolders.data.appendingPathComponent("elsewhere.db").path
            names = await Task.detached { (try? categories(dbPath: path)) ?? [] }.value
        }
    }
}
