import SwiftUI

struct EditView: View {
    var body: some View {
        NavigationStack {
            ContentUnavailableView("Nothing to edit", systemImage: "slider.horizontal.3",
                                   description: Text("Select a photo in the library to edit it."))
                .navigationTitle("Edit")
        }
    }
}
