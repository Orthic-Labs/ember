import PhotosUI
import SwiftUI
import UniformTypeIdentifiers

/// Library: the library location is a user-chosen document folder (decision D6), or the procedural
/// sample library. Shows the grid and routes to the photo viewer/editor.
struct LibraryView: View {
    @Environment(LibraryStore.self) private var store
    @State private var choosingFolder = false
    @State private var importingFiles = false
    @State private var pickedItems: [PhotosPickerItem] = []
    @State private var path: [UInt64] = []

    var body: some View {
        NavigationStack(path: $path) {
            Group {
                if store.engine == nil {
                    chooser
                } else if store.photos.isEmpty {
                    ContentUnavailableView {
                        Label("No photos yet", systemImage: "photo.badge.plus")
                    } description: {
                        Text("Import photos from your photo library or Files.")
                    }
                } else {
                    PhotoGrid(path: $path)
                }
            }
            .navigationTitle(title)
            .navigationDestination(for: UInt64.self) { id in PhotoDetailView(photoID: id) }
            .toolbar { if store.engine != nil { libraryMenu } }
            .overlay { if let message = store.busyMessage { BusyOverlay(message: message) } }
            .fileImporter(isPresented: $choosingFolder, allowedContentTypes: [.folder]) { result in
                if case let .success(url) = result { Task { await store.open(.folder(url)) } }
            }
            .fileImporter(isPresented: $importingFiles, allowedContentTypes: [.image], allowsMultipleSelection: true) { result in
                guard case let .success(urls) = result else { return }
                Task {
                    let scoped = urls.filter { $0.startAccessingSecurityScopedResource() }
                    await store.importFiles(urls)
                    scoped.forEach { $0.stopAccessingSecurityScopedResource() }
                }
            }
            .onChange(of: pickedItems) { _, items in
                guard !items.isEmpty else { return }
                pickedItems = []
                Task { await importPicked(items) }
            }
        }
    }

    private var title: String {
        switch store.location {
        case .sample: "Sample Library"
        case let .folder(url): url.lastPathComponent
        case nil: "Library"
        }
    }

    private var chooser: some View {
        ContentUnavailableView {
            Label("No library", systemImage: "photo.on.rectangle")
        } description: {
            Text("Choose a folder to hold your Ember library, or explore the sample library.")
        } actions: {
            Button("Choose Folder") { choosingFolder = true }
                .buttonStyle(.borderedProminent)
            Button("Open Sample Library") { Task { await store.open(.sample) } }
        }
    }

    private var libraryMenu: some ToolbarContent {
        ToolbarItemGroup(placement: .topBarTrailing) {
            PhotosPicker(selection: $pickedItems, matching: .images) {
                Label("Import from Photos", systemImage: "plus")
            }
            Menu {
                Button("Import from Files…", systemImage: "folder") { importingFiles = true }
                Button("Open Another Library…", systemImage: "books.vertical") { choosingFolder = true }
                Button("Close Library", systemImage: "xmark.circle") { Task { await store.forgetLibrary() } }
            } label: {
                Label("More", systemImage: "ellipsis.circle")
            }
        }
    }

    /// Photos picker items arrive as data; write each to a temporary file and import those.
    private func importPicked(_ items: [PhotosPickerItem]) async {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("Picked-\(UUID().uuidString)", isDirectory: true)
        try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        var urls: [URL] = []
        for (index, item) in items.enumerated() {
            guard let data = try? await item.loadTransferable(type: Data.self) else { continue }
            let ext = item.supportedContentTypes.first?.preferredFilenameExtension ?? "jpg"
            let url = folder.appendingPathComponent("Photo-\(index + 1).\(ext)")
            if (try? data.write(to: url)) != nil { urls.append(url) }
        }
        await store.importFiles(urls)
        try? FileManager.default.removeItem(at: folder)
    }
}

private struct PhotoGrid: View {
    @Environment(LibraryStore.self) private var store
    @Binding var path: [UInt64]

    var body: some View {
        ScrollView {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: 110), spacing: 2)], spacing: 2) {
                ForEach(store.photos) { photo in
                    Button { path.append(photo.id) } label: { PhotoCell(photo: photo) }
                        .buttonStyle(.plain)
                        .accessibilityLabel(photo.fileName)
                        .accessibilityValue(photo.edited ? "edited" : "")
                }
            }
        }
        .refreshable { await store.reload() }
    }
}

private struct PhotoCell: View {
    @Environment(LibraryStore.self) private var store
    let photo: PhotoSummary

    var body: some View {
        Color(.secondarySystemBackground)
            .aspectRatio(1, contentMode: .fit)
            .overlay {
                if let image = store.thumbnails[photo.id] {
                    Image(uiImage: image).resizable().scaledToFill()
                } else {
                    ProgressView()
                }
            }
            .clipped()
            .overlay(alignment: .bottomLeading) { badges }
            .task(id: photo.id) { await store.thumbnail(for: photo) }
    }

    @ViewBuilder private var badges: some View {
        HStack(spacing: 2) {
            if photo.rating > 0 {
                Text(String(repeating: "★", count: Int(photo.rating)))
            }
            switch photo.flag {
            case .pick: Image(systemName: "flag.fill")
            case .reject: Image(systemName: "xmark")
            case .none: EmptyView()
            }
            if photo.edited { Image(systemName: "slider.horizontal.3") }
        }
        .font(.caption2)
        .foregroundStyle(.white)
        .shadow(radius: 2)
        .padding(4)
    }
}

struct BusyOverlay: View {
    let message: String

    var body: some View {
        VStack(spacing: 12) {
            ProgressView()
            Text(message).font(.callout)
        }
        .padding(24)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14))
    }
}
