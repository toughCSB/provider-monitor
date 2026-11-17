import SwiftUI

struct ManagedAccountsSection: View {
    @ObservedObject var controller: ManagedAccountController

    var body: some View {
        Section(L10n.t("Registered monitoring accounts")) {
            Text(L10n.t("Add up to two accounts for each provider using its official CLI sign-in. Provider Monitor manages the folders; your normal CLI login and running sessions stay as they are."))
                .font(.caption).foregroundStyle(.secondary)
            Text(L10n.t("After confirmation, this provider shows your registered accounts. Browser and existing-folder sources remain available when no registered accounts are selected."))
                .font(.caption).foregroundStyle(.secondary)
            ForEach(MonitoredAccounts.Family.allCases, id: \.rawValue) { family in
                VStack(alignment: .leading, spacing: 10) {
                    HStack {
                        Text(family.name).font(.headline)
                        Spacer()
                        Button(L10n.t("Add \(family.name) account…")) { controller.begin(family) }
                            .disabled(!controller.canAdd(family))
                    }
                    let records = controller.accounts(for: family)
                    if records.isEmpty {
                        Text(L10n.t("No independently registered account yet."))
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    ForEach(records) { record in
                        HStack(alignment: .top, spacing: 12) {
                            ProviderGlyphView(glyph: family == .codex ? .openai : .claude, size: 20,
                                              accountNumber: record.slot)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(record.name).font(.subheadline.weight(.semibold))
                                Text(record.identity.email).font(.caption).textSelection(.enabled)
                                if let plan = record.identity.plan { Text(plan).font(.caption).foregroundStyle(.secondary) }
                            }
                            Spacer()
                            Button(L10n.t("Sign in again")) { controller.begin(family, replacing: record) }
                            Button(L10n.t("Remove registration")) { controller.remove(record) }
                        }
                        .disabled(controller.busyFamily != nil || controller.pending != nil)
                    }
                }
            }
            if let family = controller.busyFamily {
                HStack {
                    ProgressView().controlSize(.small)
                    Text(L10n.t("Finish \(family.name) sign-in in your browser."))
                    Spacer()
                    Button(L10n.t("Cancel")) { controller.cancel() }
                }
            }
            if let pending = controller.pending {
                VStack(alignment: .leading, spacing: 8) {
                    Text(L10n.t("Confirm the signed-in account")).font(.headline)
                    Text("\(pending.family.name) · \(pending.identity.email)").textSelection(.enabled)
                    if let plan = pending.identity.plan { Text(plan).font(.caption).foregroundStyle(.secondary) }
                    HStack {
                        Button(L10n.t("Monitor this account")) { Task { await controller.confirm() } }
                            .disabled(controller.isConfirming)
                        Button(L10n.t("Cancel")) { controller.cancel() }
                    }
                }
            }
            if let error = controller.errorMessage {
                Text(error).font(.caption).foregroundStyle(.red)
            }
        }
    }
}
