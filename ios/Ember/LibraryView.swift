import SwiftUI
import UniformTypeIdentifiers

/// Library: the library location is a user-chosen document folder (decision D6).
/// The folder bookmark and read-only index (RightKitDB) are not wired in this scaffold.
struct LibraryView: View {
    @State private var showingPicker = false
    @State private var chosenFolder: URL?

    var body: some View {
        NavigationStack {
            Group {
                if let chosenFolder {
                    Text(chosenFolder.lastPathComponent)
                        .foregroundStyle(.secondary)
                } else {
                    ContentUnavailableView {
                        Label("No library", systemImage: "photo.on.rectangle")
                    } description: {
                        Text("Choose a folder to hold your Ember library.")
                    } actions: {
                        Button("Choose Folder") { showingPicker = true }
                            .buttonStyle(.borderedProminent)
                    }
                }
            }
            .navigationTitle("Library")
            .fileImporter(isPresented: $showingPicker, allowedContentTypes: [.folder]) { result in
                if case let .success(url) = result { chosenFolder = url }
            }
        }
    }
}
