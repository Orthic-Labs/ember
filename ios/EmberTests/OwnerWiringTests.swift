import RightKitAppleCredentials
import RightKitDiagnostics
import XCTest
@testable import Ember

@MainActor
final class OwnerWiringTests: XCTestCase {
    func testOwnersAreBound() {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let wiring = OwnerWiring(
            credentials: AppleCredentialStore(service: "test.ember", backend: InMemoryAppleCredentialBackend()),
            diagnosticsURL: dir.appendingPathComponent("ember.jsonl"),
            licenseBaseURL: nil
        )
        XCTAssertEqual(wiring.credentials.service, "test.ember")
        XCTAssertEqual(wiring.diagnostics.fileURL.lastPathComponent, "ember.jsonl")
        XCTAssertEqual(wiring.storeConfiguration.groupID, OwnerWiring.appGroupID)
        XCTAssertNil(wiring.licenseController)
    }
}
