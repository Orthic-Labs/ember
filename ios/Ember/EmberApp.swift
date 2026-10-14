import SwiftUI

@main
struct EmberApp: App {
    @State private var owners = OwnerWiring()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(owners)
        }
    }
}
