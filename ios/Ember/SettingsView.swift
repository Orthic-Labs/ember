import RightKitLicense
import SwiftUI

struct SettingsView: View {
    @Environment(OwnerWiring.self) private var owners

    var body: some View {
        NavigationStack {
            Form {
                Section("Licence") {
                    if let controller = owners.licenseController {
                        LicenceStatusRow(controller: controller)
                    } else {
                        Text("Licensing is not configured in this build.")
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .navigationTitle("Settings")
        }
    }
}

private struct LicenceStatusRow: View {
    @ObservedObject var controller: LicenseController

    var body: some View {
        if controller.license == nil {
            Text("No licence activated.")
        } else {
            Text("Licence activated.")
        }
    }
}
