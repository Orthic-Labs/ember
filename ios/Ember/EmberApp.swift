import SwiftUI

@main
struct EmberApp: App {
    @State private var owners = OwnerWiring()
    @State private var library = LibraryStore()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(owners)
                .environment(library)
                .task { await library.restoreLastLibrary() }
                .alert("Something went wrong", isPresented: Binding(
                    get: { library.errorMessage != nil },
                    set: { if !$0 { library.errorMessage = nil } }
                )) {
                    Button("OK", role: .cancel) {}
                } message: {
                    Text(library.errorMessage ?? "")
                }
        }
        .onChange(of: scenePhase) { _, phase in
            // Persist completed edits before iOS may suspend the app.
            if phase == .background { Task { await library.persist() } }
        }
    }
}
