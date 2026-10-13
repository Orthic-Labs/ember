import SwiftUI

struct RootView: View {
    var body: some View {
        TabView {
            LibraryView()
                .tabItem { Label("Library", systemImage: "photo.on.rectangle") }
            EditView()
                .tabItem { Label("Edit", systemImage: "slider.horizontal.3") }
            SettingsView()
                .tabItem { Label("Settings", systemImage: "gearshape") }
        }
    }
}
