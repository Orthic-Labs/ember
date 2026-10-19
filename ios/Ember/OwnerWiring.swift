import Foundation
import Observation
import RightKitAppleCredentials
import RightKitDiagnostics
import RightKitLicense
import RightKitStore

/// Composition root: binds the app to the RightKit Apple owners that exist at the pinned tag.
/// No business logic lives here.
///
/// Owner map (Table 1-Apple):
/// - licence: `RightKitLicense` (`LicenseController` over `KeychainLicenseStore`)
/// - secrets/Keychain: `RightKitAppleCredentials` (`AppleCredentialStore`)
/// - logs: `RightKitDiagnostics` (`JSONLDiagnosticStore`)
/// - store: `RightKitStore` (`AppGroupConfiguration`, `RevisionedStateStore`)
/// - library index: Ember's Rust catalog (`crates/mobile`), not `RightKitDB`; RightKit products are
///   adopted only where they cover what the app does (Rk3 Win ruling, ios-349 RULINGS.md).
@MainActor
@Observable
final class OwnerWiring {
    static let appID = "ember"
    static let appGroupID = "group.com.orthiclabs.ember"

    let credentials: AppleCredentialStore
    let diagnostics: JSONLDiagnosticStore
    let storeConfiguration: AppGroupConfiguration
    let licenseController: LicenseController?

    init(
        credentials: AppleCredentialStore = AppleCredentialStore(service: "com.orthiclabs.ember.credentials"),
        diagnosticsURL: URL = OwnerWiring.defaultDiagnosticsURL(),
        licenseBaseURL: URL? = OwnerWiring.configuredLicenseBaseURL()
    ) {
        self.credentials = credentials
        self.diagnostics = JSONLDiagnosticStore(fileURL: diagnosticsURL)
        self.storeConfiguration = AppGroupConfiguration(groupID: Self.appGroupID, lockName: "ember")
        let licenseStore = KeychainLicenseStore(service: "com.orthiclabs.ember.license")
        // The licence device id is RightKit's persisted Keychain id. If it cannot be read or stored,
        // licensing stays unavailable (fail closed) instead of binding to a throw-away id.
        if let licenseBaseURL, let deviceID = try? licenseStore.requireDeviceID() {
            let client = LicenseActivationClient(
                configuration: LicenseClientConfiguration(
                    app: Self.appID,
                    baseURL: licenseBaseURL,
                    appVersion: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.0.0",
                    deviceID: { deviceID }
                ),
                store: licenseStore
            )
            self.licenseController = LicenseController(client: client)
        } else {
            self.licenseController = nil
        }
    }

    nonisolated static func defaultDiagnosticsURL() -> URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("Diagnostics", isDirectory: true).appendingPathComponent("ember.jsonl")
    }

    nonisolated static func configuredLicenseBaseURL() -> URL? {
        guard let raw = Bundle.main.object(forInfoDictionaryKey: "EmberLicenseBaseURL") as? String, !raw.isEmpty else {
            return nil
        }
        return URL(string: raw)
    }
}
