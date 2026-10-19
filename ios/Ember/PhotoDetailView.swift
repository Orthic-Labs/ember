import SwiftUI

/// Photo viewer with culling controls and a compact editing panel. On iPhone the adjustments sit in
/// a bottom panel under the image; on iPad (regular width) they sit in a side inspector.
struct PhotoDetailView: View {
    @Environment(LibraryStore.self) private var store
    @Environment(\.horizontalSizeClass) private var sizeClass
    let photoID: UInt64

    @State private var editing = false
    @State private var showingBefore = false
    @State private var shareURL: URL?
    @State private var canvasSize: CGSize = .zero

    private var photo: PhotoSummary? { store.photos.first { $0.id == photoID } }

    var body: some View {
        Group {
            if sizeClass == .regular {
                HStack(spacing: 0) {
                    canvas
                    if editing { AdjustmentsPanel(size: canvasSize).frame(width: 340) }
                }
            } else {
                VStack(spacing: 0) {
                    canvas
                    if editing { AdjustmentsPanel(size: canvasSize).frame(maxHeight: 320) }
                }
            }
        }
        .navigationTitle(photo?.fileName ?? "Photo")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { toolbar }
        .safeAreaInset(edge: .bottom) { if !editing, let photo { CullBar(photo: photo) } }
        .overlay { if let message = store.busyMessage { BusyOverlay(message: message) } }
        .sheet(item: $shareURL) { url in ShareSheet(items: [url]) }
        .task(id: photoID) {
            guard canvasSize != .zero else { return }
            await store.activate(photoID, size: canvasSize)
        }
    }

    private var canvas: some View {
        GeometryReader { geometry in
            ZStack {
                Color.black
                if let image = store.loupePhotoID == photoID ? store.loupe : store.thumbnails[photoID] {
                    Image(uiImage: image).resizable().scaledToFit()
                } else {
                    ProgressView().tint(.white)
                }
                if showingBefore {
                    Text("Before").font(.caption.bold()).padding(6)
                        .background(.thinMaterial, in: Capsule())
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading).padding()
                }
            }
            .onAppear { updateSize(geometry.size) }
            .onChange(of: geometry.size) { _, size in updateSize(size) }
            // Press and hold the photo to compare with the unedited original.
            .onLongPressGesture(minimumDuration: 0.2, maximumDistance: 40) {
            } onPressingChanged: { pressing in
                showingBefore = pressing
                Task { await store.renderLoupe(size: canvasSize, quality: .full, before: pressing) }
            }
            .accessibilityLabel(photo?.fileName ?? "Photo")
            .accessibilityHint("Touch and hold to see the original.")
        }
    }

    private func updateSize(_ size: CGSize) {
        let first = canvasSize == .zero
        canvasSize = size
        if first { Task { await store.activate(photoID, size: size) } }
    }

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        ToolbarItemGroup(placement: .topBarTrailing) {
            if editing {
                Button("Undo", systemImage: "arrow.uturn.backward") { Task { await store.undo(size: canvasSize) } }
                    .disabled((store.snapshot?.undo ?? 0) == 0)
                Button("Redo", systemImage: "arrow.uturn.forward") { Task { await store.redo(size: canvasSize) } }
                    .disabled((store.snapshot?.redo ?? 0) == 0)
            }
            Button("Share", systemImage: "square.and.arrow.up") {
                Task { shareURL = await store.exportJPEG(photoID) }
            }
            Button(editing ? "Done" : "Edit") { withAnimation { editing.toggle() } }
                .fontWeight(editing ? .semibold : .regular)
        }
    }
}

/// Stars and pick/reject for the photo on screen.
private struct CullBar: View {
    @Environment(LibraryStore.self) private var store
    let photo: PhotoSummary

    var body: some View {
        HStack {
            ForEach(1...5, id: \.self) { star in
                let rating = UInt8(star)
                Button {
                    Task { await store.setRating(photo.rating == rating ? 0 : rating, for: photo.id) }
                } label: {
                    Image(systemName: photo.rating >= rating ? "star.fill" : "star")
                }
                .accessibilityLabel("\(star) star\(star == 1 ? "" : "s")")
            }
            Spacer()
            Button {
                Task { await store.setFlag(photo.flag == .pick ? .none : .pick, for: photo.id) }
            } label: {
                Image(systemName: photo.flag == .pick ? "flag.fill" : "flag")
            }
            .accessibilityLabel(photo.flag == .pick ? "Remove pick" : "Pick")
            Button {
                Task { await store.setFlag(photo.flag == .reject ? .none : .reject, for: photo.id) }
            } label: {
                Image(systemName: photo.flag == .reject ? "xmark.circle.fill" : "xmark.circle")
            }
            .accessibilityLabel(photo.flag == .reject ? "Remove reject" : "Reject")
        }
        .font(.title3)
        .padding(.horizontal)
        .padding(.vertical, 10)
        .background(.bar)
    }
}

/// Sliders built from the Rust control specs: ranges, defaults and labels come from the engine.
private struct AdjustmentsPanel: View {
    @Environment(LibraryStore.self) private var store
    let size: CGSize

    var body: some View {
        List {
            ForEach(store.sections, id: \.name) { section in
                Section(section.name) {
                    ForEach(section.controls) { control in
                        ControlSlider(control: control, size: size)
                    }
                }
            }
        }
        .listStyle(.insetGrouped)
    }
}

private struct ControlSlider: View {
    @Environment(LibraryStore.self) private var store
    let control: ControlSpec
    let size: CGSize
    @State private var draft: Double?

    var body: some View {
        let value = draft ?? store.value(of: control)
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Text(control.label)
                Spacer()
                Text(value, format: .number.precision(.fractionLength(control.decimals)))
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
            }
            .font(.subheadline)
            Slider(
                value: Binding(get: { value }, set: { newValue in
                    draft = newValue
                    store.setControl(control, to: newValue, size: size)
                }),
                in: control.min...control.max,
                step: control.step > 0 ? control.step : 0.01
            ) { editingNow in
                if !editingNow {
                    Task {
                        await store.endControlInteraction(size: size)
                        draft = nil
                    }
                }
            }
            .accessibilityLabel(control.label)
        }
        .contentShape(Rectangle())
        .onTapGesture(count: 2) { Task { await store.resetControl(control, size: size) } }
    }
}

extension URL: @retroactive Identifiable {
    public var id: String { absoluteString }
}

private struct ShareSheet: UIViewControllerRepresentable {
    let items: [Any]

    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }

    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}
