import Foundation
import Observation
import UIKit
import os

private let log = Logger(subsystem: "com.orthiclabs.ember", category: "library")

/// App state for one open library. Main-actor only: bridge results are applied here, never from the
/// queue they arrive on. Stale preview results are discarded by sequence number.
@MainActor
@Observable
final class LibraryStore {
    enum Location: Equatable {
        case sample
        case folder(URL)
    }

    private(set) var engine: EmberEngine?
    private(set) var location: Location?
    private(set) var photos: [PhotoSummary] = []
    private(set) var snapshot: Snapshot?
    private(set) var thumbnails: [UInt64: UIImage] = [:]
    private(set) var loupe: UIImage?
    private(set) var loupePhotoID: UInt64?
    private(set) var busyMessage: String?
    var errorMessage: String?

    private var sequence: UInt64 = 0
    private var latestLoupeSequence: UInt64 = 0
    private var thumbnailsInFlight: Set<UInt64> = []
    private var scopedFolder: URL?
    private var interactionOpen = false
    private var pendingControl: (ControlSpec, Double, CGSize)?
    private var controlWorker: Task<Void, Never>?

    static let bookmarkKey = "ember.library-bookmark"

    // MARK: - Opening

    /// Reopens the last library folder (decision D6: a user-chosen document folder), if any.
    func restoreLastLibrary() async {
        // UI tests start from the library chooser.
        if ProcessInfo.processInfo.arguments.contains("-EmberResetLibrary") {
            UserDefaults.standard.removeObject(forKey: Self.bookmarkKey)
            return
        }
        guard engine == nil, let data = UserDefaults.standard.data(forKey: Self.bookmarkKey) else { return }
        var stale = false
        guard let url = try? URL(resolvingBookmarkData: data, bookmarkDataIsStale: &stale) else {
            UserDefaults.standard.removeObject(forKey: Self.bookmarkKey)
            return
        }
        await open(.folder(url), remember: stale)
    }

    func open(_ location: Location, remember: Bool = true) async {
        await close()
        busyMessage = "Opening library…"
        defer { busyMessage = nil }
        var folder: URL?
        if case let .folder(url) = location {
            if url.startAccessingSecurityScopedResource() { scopedFolder = url }
            folder = url
            if remember, let data = try? url.bookmarkData() {
                UserDefaults.standard.set(data, forKey: Self.bookmarkKey)
            }
        }
        do {
            engine = try await EmberEngine.open(folder: folder)
            self.location = location
            await reload()
        } catch {
            report(error)
            releaseFolder()
        }
    }

    func close() async {
        guard let engine else { return }
        try? await engine.close()
        self.engine = nil
        location = nil
        photos = []
        snapshot = nil
        thumbnails = [:]
        loupe = nil
        loupePhotoID = nil
        releaseFolder()
    }

    /// Forgets the remembered folder so the next launch starts at the library chooser.
    func forgetLibrary() async {
        UserDefaults.standard.removeObject(forKey: Self.bookmarkKey)
        await close()
    }

    private func releaseFolder() {
        scopedFolder?.stopAccessingSecurityScopedResource()
        scopedFolder = nil
    }

    // MARK: - Library view

    func reload() async {
        guard let engine else { return }
        do {
            let snapshot = try await engine.snapshot()
            let slice = try await engine.viewSlice(generation: snapshot.viewGeneration, offset: 0, limit: 1000)
            self.snapshot = snapshot
            photos = slice.photos
        } catch {
            report(error)
        }
    }

    private func refreshSnapshot() async {
        guard let engine else { return }
        do { snapshot = try await engine.snapshot() } catch { report(error) }
    }

    func thumbnail(for photo: PhotoSummary) async {
        guard let engine, thumbnails[photo.id] == nil, !thumbnailsInFlight.contains(photo.id),
              let generation = snapshot?.viewGeneration else { return }
        thumbnailsInFlight.insert(photo.id)
        defer { thumbnailsInFlight.remove(photo.id) }
        sequence &+= 1
        let request = PreviewRequest(photoId: photo.id, slot: "grid-\(photo.id)", viewGeneration: generation,
                                     width: 360, height: 360, quality: .draft, before: false, sequence: sequence)
        if let (_, data) = try? await engine.preview(request), let data, let image = UIImage(data: data) {
            thumbnails[photo.id] = image
        }
    }

    // MARK: - Culling

    func setRating(_ rating: UInt8, for id: UInt64) async {
        await mutate(id) { try await $0.run("photo.rate", ["rating": Int(rating), "ids": [id]]) }
    }

    func setFlag(_ flag: PhotoFlag, for id: UInt64) async {
        await mutate(id) { try await $0.run("photo.flag", ["flag": flag.rawValue, "ids": [id]]) }
    }

    private func mutate(_ id: UInt64, _ body: (EmberEngine) async throws -> Void) async {
        guard let engine else { return }
        do {
            try await body(engine)
            await reload()
        } catch {
            report(error)
        }
    }

    // MARK: - Editing

    /// Makes `id` the active photo and renders it at `size` points.
    func activate(_ id: UInt64, size: CGSize) async {
        guard let engine else { return }
        do {
            try await engine.run("library.select", ["ids": [id], "active": id])
            if loupePhotoID != id { loupe = thumbnails[id] }
            loupePhotoID = id
            await refreshSnapshot()
            await renderLoupe(size: size, quality: .full)
        } catch {
            report(error)
        }
    }

    func value(of control: ControlSpec) -> Double {
        snapshot?.controlValues[control.id] ?? control.default
    }

    var sections: [(name: String, controls: [ControlSpec])] {
        // V1 mobile scope (docs/ios-architecture.md): global Light/Color adjustments plus the simple
        // single-slider sections. Curves, mixers, grading and masks stay on desktop for now.
        let order = ["Light", "Color", "Effects", "Vignette"]
        let grouped = Dictionary(grouping: (snapshot?.controls ?? []).filter { order.contains($0.section) }, by: \.section)
        return order.compactMap { name in grouped[name].map { (name: name, controls: $0) } }
    }

    /// Slider motion: records the newest value and lets one worker apply values in order, skipping
    /// ones a newer value replaced, so a slow render never reorders or reopens an interaction.
    func setControl(_ control: ControlSpec, to value: Double, size: CGSize) {
        pendingControl = (control, value, size)
        snapshot?.controlValues[control.id] = value
        guard controlWorker == nil else { return }
        controlWorker = Task { await drainControlChanges() }
    }

    private func drainControlChanges() async {
        defer { controlWorker = nil }
        while let (control, value, size) = pendingControl, let engine {
            pendingControl = nil
            do {
                if !interactionOpen {
                    interactionOpen = true
                    try await engine.run("develop.beginInteraction", ["label": control.label])
                }
                try await engine.run("develop.set", ["control": control.id, "value": value])
                log.debug("develop.set \(control.id, privacy: .public) = \(value)")
                await renderLoupe(size: size, quality: .draft)
            } catch {
                report(error)
                return
            }
        }
    }

    /// Slider released: waits for queued values, commits the interaction as one undo step and renders
    /// full quality.
    func endControlInteraction(size: CGSize) async {
        await controlWorker?.value
        guard let engine, interactionOpen else { return }
        interactionOpen = false
        do {
            try await engine.run("develop.endInteraction")
            await refreshSnapshot()
            await renderLoupe(size: size, quality: .full)
            if let id = loupePhotoID { thumbnails[id] = nil }
            await reloadPhotosOnly()
        } catch {
            report(error)
        }
    }

    func resetControl(_ control: ControlSpec, size: CGSize) async {
        setControl(control, to: control.default, size: size)
        await endControlInteraction(size: size)
    }

    func undo(size: CGSize) async { await history("edit.undo", size: size) }
    func redo(size: CGSize) async { await history("edit.redo", size: size) }

    private func history(_ id: String, size: CGSize) async {
        guard let engine else { return }
        do {
            try await engine.run(id)
            await refreshSnapshot()
            await renderLoupe(size: size, quality: .full)
        } catch {
            report(error)
        }
    }

    func renderLoupe(size: CGSize, quality: PreviewRequest.Quality, before: Bool = false, retry: Int = 2) async {
        guard let engine, let id = loupePhotoID, let generation = snapshot?.viewGeneration else { return }
        sequence &+= 1
        let mine = sequence
        latestLoupeSequence = mine
        let scale = UITraitCollection.current.displayScale
        let request = PreviewRequest(photoId: id, slot: "main", viewGeneration: generation,
                                     width: UInt32(clamping: Int(size.width * scale)),
                                     height: UInt32(clamping: Int(size.height * scale)),
                                     quality: quality, before: before, sequence: mine)
        do {
            let (descriptor, data) = try await engine.preview(request)
            log.debug("loupe seq \(mine) \(quality.rawValue, privacy: .public) provisional \(descriptor.provisional ?? false) source \(descriptor.source ?? "-", privacy: .public) -> \(descriptor.width)x\(descriptor.height) bytes \(data?.count ?? -1) latest \(self.latestLoupeSequence)")
            guard mine == latestLoupeSequence, loupePhotoID == id, let data, let image = UIImage(data: data) else { return }
            loupe = image
        } catch let EmberError.Session(message) where Self.isSuperseded(message) {
            log.debug("loupe seq \(mine) superseded: \(message)")
            // "superseded": a newer request for this slot is already on its way. "stale": the view
            // changed under this request (a rating, a filter); if nothing newer was asked for, ask
            // again with the current generation, or the viewer would keep its stand-in image.
            if message.contains("stale"), mine == latestLoupeSequence, retry > 0 {
                await refreshSnapshot()
                await renderLoupe(size: size, quality: quality, before: before, retry: retry - 1)
            }
        } catch {
            log.error("loupe seq \(mine) failed: \(userMessage(for: error))")
            report(error)
        }
    }

    /// Preview errors that only mean a newer request replaced this one.
    nonisolated static func isSuperseded(_ message: String) -> Bool {
        message.contains("stale") || message.contains("superseded")
    }

    private func reloadPhotosOnly() async {
        guard let engine, let generation = snapshot?.viewGeneration else { return }
        if let slice = try? await engine.viewSlice(generation: generation, offset: 0, limit: 1000) {
            photos = slice.photos
        }
    }

    // MARK: - Import & export

    /// Copies picked files into the library's Originals folder (the source stays untouched), then
    /// imports them by path.
    func importFiles(_ urls: [URL]) async {
        guard let engine, !urls.isEmpty else { return }
        busyMessage = "Importing \(urls.count) photo\(urls.count == 1 ? "" : "s")…"
        defer { busyMessage = nil }
        do {
            let originals = try originalsFolder()
            var paths: [String] = []
            for url in urls {
                let destination = originals.appendingPathComponent("\(UUID().uuidString.prefix(8))-\(url.lastPathComponent)")
                try FileManager.default.copyItem(at: url, to: destination)
                paths.append(destination.path)
            }
            try await engine.run("library.import", ["paths": paths])
            try await waitForJobs { !$0.status.importing }
            await reload()
        } catch {
            report(error)
        }
    }

    private func originalsFolder() throws -> URL {
        let base: URL
        if case let .folder(url) = location {
            base = url
        } else {
            base = FileManager.default.temporaryDirectory.appendingPathComponent("EmberSample", isDirectory: true)
        }
        let folder = base.appendingPathComponent("Originals", isDirectory: true)
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        return folder
    }

    /// Exports `id` as a JPEG into a fresh temporary folder and returns the file for sharing.
    func exportJPEG(_ id: UInt64) async -> URL? {
        guard let engine else { return nil }
        busyMessage = "Exporting…"
        defer { busyMessage = nil }
        do {
            let folder = FileManager.default.temporaryDirectory.appendingPathComponent("Export-\(UUID().uuidString)", isDirectory: true)
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            try await engine.run("app.export", ["ids": [id], "dir": folder.path, "format": "jpeg", "quality": 90])
            try await waitForJobs { !$0.status.exporting }
            if let failed = snapshot?.status.completedJobs.last(where: { $0.kind == "export" && $0.error != nil }) {
                errorMessage = failed.error
                return nil
            }
            let files = try FileManager.default.contentsOfDirectory(at: folder, includingPropertiesForKeys: nil)
            guard let file = files.first else { throw EngineFailure.exportProducedNoFile }
            return file
        } catch {
            report(error)
            return nil
        }
    }

    /// Polls the snapshot until `done` holds (jobs report through it), at most two minutes.
    private func waitForJobs(_ done: (Snapshot) -> Bool) async throws {
        guard let engine else { return }
        for _ in 0..<1200 {
            let current = try await engine.snapshot()
            snapshot = current
            if done(current) { return }
            try await Task.sleep(for: .milliseconds(100))
        }
    }

    // MARK: - Lifecycle

    func persist() async {
        try? await engine?.persist()
    }

    private func report(_ error: Error) {
        errorMessage = userMessage(for: error)
    }
}
