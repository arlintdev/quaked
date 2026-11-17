// Lighting (the knob's light ring) and General settings.

import AppKit
import SwiftUI

struct LightingView: View {
    @EnvironmentObject var store: ConfigStore

    private var ring: Binding<Ring> { $store.config.ring }

    var body: some View {
        Form {
            Section {
                Toggle("Drive the knob's light ring", isOn: ring.enabled)
                LabeledContent("Brightness") {
                    HStack {
                        Slider(value: Binding(get: { Double(store.config.ring.brightness) },
                                              set: { store.config.ring.brightness = Int($0.rounded()) }),
                               in: 0...255)
                        Text("\(store.config.ring.brightness)")
                            .monospacedDigit()
                            .frame(width: 36, alignment: .trailing)
                    }
                }
                .disabled(!store.config.ring.enabled)
            } footer: {
                Text("Off leaves the ring as the device has it.").font(.caption).foregroundColor(.secondary)
            }

            Section("Page colors") {
                ForEach(PageId.allCases) { page in
                    HexColorField(title: page.label, hex: ring[dynamicMember: Ring.keyPath(for: page)])
                }
            }
            .disabled(!store.config.ring.enabled)

            Section("Alert") {
                Toggle(isOn: ring.alert) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Alert")
                        Text("Breathes while a Claude session needs you.").font(.caption).foregroundColor(.secondary)
                    }
                }
                HexColorField(title: "Alert color", hex: ring.alertColor)
                    .disabled(!store.config.ring.alert)
            }
            .disabled(!store.config.ring.enabled)
        }
        .formStyle(.grouped)
    }
}

struct GeneralView: View {
    @EnvironmentObject var store: ConfigStore
    /// All four pages in display order, shown pages first; hidden ones keep their place
    /// here while the window is open.
    @State private var order: [PageId] = []

    private var shown: [PageId] { store.config.pages }

    var body: some View {
        Form {
            Section {
                Toggle(isOn: $store.config.buzzer) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Buzzer")
                        Text("Chirps when a Claude session starts waiting on you.").font(.caption).foregroundColor(.secondary)
                    }
                }
                Stepper(value: $store.config.idleSeconds, in: 5...3600, step: 5) {
                    LabeledContent("Back to the top after") {
                        Text("\(store.config.idleSeconds) s idle").monospacedDigit()
                    }
                }
            }

            Section {
                ForEach(Array(order.enumerated()), id: \.element) { index, page in
                    HStack {
                        Toggle(page.label, isOn: included(page))
                            .disabled(shown == [page])
                        Spacer()
                        Button { move(index, by: -1) } label: { Image(systemName: "chevron.up") }
                            .disabled(index == 0)
                        Button { move(index, by: 1) } label: { Image(systemName: "chevron.down") }
                            .disabled(index == order.count - 1)
                    }
                    .buttonStyle(.borderless)
                }
                .onMove { order.move(fromOffsets: $0, toOffset: $1); commit() }
            } header: {
                Text("Pages")
            } footer: {
                Text("The knob steps through the pages in this order. Untick one to hide it; at least one stays.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Section {
                Toggle("Accept the match automatically", isOn: $store.config.league.autoAccept)
                Stepper(value: $store.config.league.acceptDelayMs, in: 0...8000, step: 250) {
                    LabeledContent("Wait before accepting") {
                        Text(String(format: "%.2g s", Double(store.config.league.acceptDelayMs) / 1000)).monospacedDigit()
                    }
                }
                Toggle("Jump to the League page when the queue pops", isOn: $store.config.league.snap)
            } header: {
                Text("League of Legends")
            } footer: {
                Text("Uses the League client's local API while it's running (live or PBE).")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Section {
                TextField("Socket", text: $store.config.docker.socket, prompt: Text("Automatic (Kiri first)"))
            } header: {
                Text("Docker")
            } footer: {
                Text("Leave empty to use Kiri's ~/.kiri/run/docker.sock, or point at another Docker-compatible engine.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Section {
                Toggle("Built-in microphone", isOn: $store.config.device.mic)
            } header: {
                Text("Quake")
            } footer: {
                Text("The Quake's own mic. Turning it off also turns off its indicator light. Your call mic is set on the Soundboard page.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Section {
                Toggle("Flip touch left / right", isOn: $store.config.touch.flipX)
                Toggle("Flip touch top / bottom", isOn: $store.config.touch.flipY)
            } header: {
                Text("Touch")
            } footer: {
                Text("Only change these if taps land on the wrong spot. The Quake's panel normally needs top / bottom flipped.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Section("Files") {
                LabeledContent("Settings") {
                    HStack {
                        Text((ConfigStore.url.path as NSString).abbreviatingWithTildeInPath)
                            .textSelection(.enabled)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Button("Show in Finder") {
                            if store.fileExists {
                                NSWorkspace.shared.activateFileViewerSelecting([ConfigStore.url])
                            } else {
                                NSWorkspace.shared.open(ConfigStore.url.deletingLastPathComponent())
                            }
                        }
                        .disabled(!FileManager.default.fileExists(atPath: ConfigStore.url.deletingLastPathComponent().path))
                    }
                }
            }
        }
        .formStyle(.grouped)
        .onAppear(perform: syncOrder)
        .onChange(of: store.config.pages) { _ in syncOrder() }
    }

    /// Rebuilds `order` when the saved pages no longer match it (first load, outside edits).
    private func syncOrder() {
        if order.filter(shown.contains) != shown || order.count != PageId.allCases.count {
            order = shown + PageId.allCases.filter { !shown.contains($0) }
        }
    }

    private func included(_ page: PageId) -> Binding<Bool> {
        Binding(get: { shown.contains(page) },
                set: { on in
                    let next = order.filter { $0 == page ? on : shown.contains($0) }
                    if !next.isEmpty { store.config.pages = next }
                })
    }

    private func move(_ index: Int, by delta: Int) {
        let target = index + delta
        guard order.indices.contains(target) else { return }
        order.swapAt(index, target)
        commit()
    }

    private func commit() {
        let next = order.filter(shown.contains)
        if !next.isEmpty && next != shown { store.config.pages = next }
    }
}
