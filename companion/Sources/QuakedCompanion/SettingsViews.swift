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
                LabeledContent("Hosts") {
                    LinesEditor(lines: $store.config.ops.hosts)
                }
                Toggle("Read load, memory, disk and GPU over SSH", isOn: $store.config.ops.sshStats)
                LabeledContent("Sites") {
                    LinesEditor(lines: $store.config.ops.sites)
                }
            } header: {
                Text("Ops")
            } footer: {
                Text("Hosts are SSH aliases from ~/.ssh/config, one per line; stats use key login only. Sites are checked for status, response time and certificate expiry.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Section {
                Toggle("Blink the ring when something is new", isOn: $store.config.alerts.blink)
                Toggle("Jump to the page that has it", isOn: $store.config.alerts.jump)
                Stepper(value: $store.config.alerts.seconds, in: 10...3600, step: 30) {
                    LabeledContent("Stop blinking after") {
                        Text("\(store.config.alerts.seconds / 60) min \(store.config.alerts.seconds % 60) s").monospacedDigit()
                    }
                }
                Toggle("Claude sessions waiting on you", isOn: $store.config.alerts.ai)
                Toggle("Review requests, failing CI, approvals, conflicts", isOn: $store.config.alerts.code)
                Toggle("Hosts going down, sites failing", isOn: $store.config.alerts.ops)
                Toggle("Containers stopping", isOn: $store.config.alerts.docker)
            } header: {
                Text("New stuff")
            } footer: {
                Text("Touching the board stops the blinking. It won't jump while you've used the board in the last 15 seconds.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Section {
                Toggle("Share sessions with my other Mac", isOn: $store.config.sync.enabled)
                LabeledContent("Pairing key") {
                    HStack {
                        TextField("", text: $store.config.sync.key, prompt: Text("paste the other Mac's key"))
                            .font(.system(.body, design: .monospaced))
                            .textFieldStyle(.roundedBorder)
                            .frame(maxWidth: 200)
                        Button("Copy") {
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(store.config.sync.key, forType: .string)
                        }
                        .disabled(store.config.sync.key.isEmpty)
                    }
                }
                TextField("Name on the network", text: $store.config.sync.name, prompt: Text("This Mac's computer name"))
            } header: {
                Text("Sync")
            } footer: {
                Text("Both Macs run quaked and find each other with Bonjour on the local network. Give them the same pairing key; the Mac with the Quake attached shows the other's Claude sessions too.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Section {
                Toggle("Include GitLab merge requests (glab)", isOn: $store.config.code.gitlab)
                Stepper(value: $store.config.code.refreshSeconds, in: 15...900, step: 15) {
                    LabeledContent("Refresh every") {
                        Text("\(store.config.code.refreshSeconds) s").monospacedDigit()
                    }
                }
            } header: {
                Text("Code")
            } footer: {
                Text("Your open pull requests and review requests through the gh CLI's login, and GitLab merge requests through glab's. Twice as often while the page is showing.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Section {
                LabeledContent("Screen brightness") {
                    HStack {
                        Slider(value: Binding(get: { Double(store.config.device.screenBrightness ?? 255) },
                                              set: { store.config.device.screenBrightness = Int($0.rounded()) }),
                               in: 10...255)
                        Text("\(Int((Double(store.config.device.screenBrightness ?? 255) / 255 * 100).rounded()))%")
                            .monospacedDigit()
                            .frame(width: 44, alignment: .trailing)
                    }
                }
                Toggle("Built-in microphone", isOn: $store.config.device.mic)
            } header: {
                Text("Quake")
            } footer: {
                Text("Brightness also lives on the board's settings page (the gear). The Quake's own mic: turning it off also turns off its indicator light. Your call mic is set on the Soundboard page.")
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
    }
}

/// One entry per line; blank lines are dropped when saved. Keeps its own text so a
/// newline being typed isn't trimmed away mid-edit.
struct LinesEditor: View {
    @Binding var lines: [String]
    @State private var text = ""
    @State private var loaded = false

    var body: some View {
        TextEditor(text: $text)
            .font(.system(.body, design: .monospaced))
            .frame(minHeight: 84)
            .onAppear {
                if !loaded {
                    text = lines.joined(separator: "\n")
                    loaded = true
                }
            }
            .onChange(of: text) { value in
                let parsed = value.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
                if parsed != lines {
                    lines = parsed
                }
            }
    }
}
