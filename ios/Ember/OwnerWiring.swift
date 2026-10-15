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
/// - library index (read-only): `RightKitDB`, present after tag 0.7.0; wired when the pin moves.
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
        if let licenseBaseURL {
            let client = LicenseActivationClient(
                configuration: LicenseClientConfiguration(
                    app: Self.appID,
                    baseURL: licenseBaseURL,
                    appVersion: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.0.0",
                    deviceID: { UIDeviceID.current }
                ),
                store: KeychainLicenseStore(service: "com.orthiclabs.ember.license")
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

enum UIDeviceID {
    /// Per-install device identifier; the licence owner treats it as an opaque string.
    static var current: String {
        let key = "ember.device-id"
        if let existing = UserDefaults.standard.string(forKey: key) { return existing }
        let fresh = UUID().uuidString
        UserDefaults.standard.set(fresh, forKey: key)
        return fresh
    }
}
