import Foundation

/// Swift face of the Rust mobile bridge (`crates/mobile`). Every bridge call blocks, so each one runs
/// on a background queue and resumes the caller; the Rust session owner serializes them.
/// No UI state lives here: `LibraryStore` applies results on the main actor.
final class EmberEngine: Sendable {
    private let session: EmberSession
    private let queue = DispatchQueue(label: "com.orthiclabs.ember.engine", qos: .userInitiated, attributes: .concurrent)

    private init(session: EmberSession) {
        self.session = session
    }

    /// Opens a library folder, or the procedural sample library when `folder` is nil.
    static func open(folder: URL?, demoCount: UInt32 = 24) async throws -> EmberEngine {
        try await Self.background {
            let options = SessionOptions(libraryPath: folder?.path, demo: folder == nil, demoCount: folder == nil ? demoCount : 0)
            return EmberEngine(session: try EmberSession.open(options: options))
        }
    }

    func snapshot() async throws -> Snapshot {
        try await call { try Self.decode(Snapshot.self, $0.snapshot()) }
    }

    func viewSlice(generation: UInt64?, offset: Int, limit: Int) async throws -> ViewSlice {
        let offset = UInt32(clamping: offset), limit = UInt32(clamping: limit)
        return try await call { try Self.decode(ViewSlice.self, $0.viewSlice(generation: generation, offset: offset, limit: limit)) }
    }

    /// Runs a command by id with JSON-object parameters and returns its JSON result.
    @discardableResult
    func run(_ id: String, _ params: [String: Any] = [:]) async throws -> Data {
        let json = try Self.encode(params)
        return try await call { Data(try $0.run(id: id, paramsJson: json).utf8) }
    }

    /// Renders a preview and returns its descriptor and encoded bytes. A nil image means the handle
    /// expired before it could be read (a newer request replaced it).
    func preview(_ request: PreviewRequest, quick: Bool = false) async throws -> (PreviewDescriptor, Data?) {
        let json = try String(decoding: JSONEncoder().encode(request), as: UTF8.self)
        return try await call { session in
            let raw = quick ? try session.previewQuick(requestJson: json) : try session.preview(requestJson: json)
            let descriptor = try Self.decode(PreviewDescriptor.self, raw)
            let bytes = session.previewBytes(handle: descriptor.handle).map { Data($0.bytes) }
            _ = session.releasePreview(handle: descriptor.handle)
            return (descriptor, bytes)
        }
    }

    func persist() async throws {
        try await call { try $0.persist() }
    }

    func close() async throws {
        try await call { try $0.close() }
    }

    // MARK: - Plumbing

    private func call<T: Sendable>(_ body: @escaping @Sendable (EmberSession) throws -> T) async throws -> T {
        let session = self.session
        return try await withCheckedThrowingContinuation { continuation in
            queue.async { continuation.resume(with: Result { try body(session) }) }
        }
    }

    private static func background<T: Sendable>(_ body: @escaping @Sendable () throws -> T) async throws -> T {
        try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async { continuation.resume(with: Result { try body() }) }
        }
    }

    private static func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
        try JSONDecoder().decode(type, from: Data(json.utf8))
    }

    private static func encode(_ params: [String: Any]) throws -> String {
        guard JSONSerialization.isValidJSONObject(params) else { throw EngineFailure.invalidParameters }
        return String(decoding: try JSONSerialization.data(withJSONObject: params), as: UTF8.self)
    }
}

enum EngineFailure: LocalizedError {
    case invalidParameters
    case exportProducedNoFile

    var errorDescription: String? {
        switch self {
        case .invalidParameters: "The request could not be encoded."
        case .exportProducedNoFile: "Export finished without writing a file."
        }
    }
}

/// The message to show for an error. UniFFI's own `LocalizedError` text is a debug description, so
/// bridge errors are mapped to their Rust messages here.
func userMessage(for error: Error) -> String {
    switch error {
    case let EmberError.InvalidRequest(message): "Invalid request: \(message)"
    case let EmberError.Session(message): message
    case EmberError.Closed: "The library is closed."
    default: error.localizedDescription
    }
}
