import Foundation

// Decodable views of the bridge's JSON (desktop-host snapshot/view-slice/preview shapes, bridge v1).
// Only the fields the app reads are declared; unknown keys are ignored.

struct Snapshot: Decodable, Sendable {
    var viewGeneration: UInt64
    var total: Int
    var active: UInt64?
    var undo: Int
    var redo: Int
    var controls: [ControlSpec]
    var controlValues: [String: Double]
    var status: HostStatus
}

struct HostStatus: Decodable, Sendable {
    var importing: Bool
    var exporting: Bool
    var jobs: [JobStatus]
    var completedJobs: [TerminalTask]
    var error: String?
}

struct JobStatus: Decodable, Sendable, Identifiable {
    var id: String
    var kind: String
    var label: String
    var completed: Int
    var total: Int
}

struct TerminalTask: Decodable, Sendable {
    var id: String
    var kind: String
    var state: String
    var error: String?
}

struct ControlSpec: Decodable, Sendable, Identifiable, Hashable {
    var id: String
    var label: String
    var section: String
    var min: Double
    var max: Double
    var `default`: Double
    var step: Double
    var decimals: Int
}

struct ViewSlice: Decodable, Sendable {
    var generation: UInt64
    var total: Int
    var offset: Int
    var photos: [PhotoSummary]
}

enum PhotoFlag: String, Decodable, Sendable {
    case none, pick, reject

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = PhotoFlag(rawValue: raw.lowercased()) ?? .none
    }
}

struct PhotoSummary: Decodable, Sendable, Identifiable, Hashable {
    var id: UInt64
    var fileName: String
    var format: String
    var w: UInt32
    var h: UInt32
    var captured: String?
    var rating: UInt8
    var flag: PhotoFlag
    var edited: Bool
    var camera: String
    var lens: String
    var shutter: String
    var aperture: Float?
    var iso: UInt32?
    var focalMm: Float?

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.id == rhs.id && lhs.rating == rhs.rating && lhs.flag == rhs.flag && lhs.edited == rhs.edited
    }

    func hash(into hasher: inout Hasher) { hasher.combine(id) }
}

struct PreviewRequest: Encodable, Sendable {
    enum Quality: String, Encodable, Sendable { case draft, full }

    var photoId: UInt64
    var slot: String
    var viewGeneration: UInt64
    var width: UInt32
    var height: UInt32
    var quality: Quality
    var before: Bool
    var sequence: UInt64
}

struct PreviewDescriptor: Decodable, Sendable {
    var handle: String
    var photoId: UInt64
    var sequence: UInt64
    var width: UInt32
    var height: UInt32
    var renderMs: Double
    /// True when the host answered with a quick stand-in (e.g. the cached thumbnail) instead of a render.
    var provisional: Bool?
    var source: String?
}
