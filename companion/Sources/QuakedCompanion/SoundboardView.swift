// Soundboard clips (the audio files in a folder, optionally restyled) and audio routing.

import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct SoundboardView: View {
    @EnvironmentObject var store: ConfigStore
    @EnvironmentObject var daemon: DaemonModel
    @State private var items: [ClipLibrary.Item] = []
    @State private var expanded: String?

    private var sb: Binding<Soundboard> { $store.config.soundboard }
    private var folderExists: Bool {
        var isDir: ObjCBool = false
        return FileManager.default.fileExists(atPath: ClipLibrary.expand(store.config.soundboard.folder), isDirectory: &isDir)
            && isDir.boolValue
    }

    var body: some View {
        Form {
            Section("Clips folder") {
                LabeledContent("Folder") {
                    HStack {
                        TextField("Folder", text: sb.folder).labelsHidden()
                        Button("Choose…", action: chooseFolder)
                    }
                }
                HStack {
                    Button("Add clips…", action: addClips)
                    Button("Show in Finder", action: reveal).disabled(!folderExists)
                    Button("Refresh", action: rescan)
                    Spacer()
                    if !folderExists {
                        Text("The folder doesn't exist yet; Add clips… creates it.")
                            .font(.caption)
                            .foregroundColor(.secondary)
                    }
                }
            }

            Section {
                if items.isEmpty {
                    Text("No audio files here yet (wav, mp3, m4a, aac, aiff, flac or caf). Use Add clips… or drop files into the folder.")
                        .foregroundColor(.secondary)
                }
                ForEach(items) { item in
                    ClipRow(item: item, clip: clipBinding(item),
                            playing: daemon.status?.soundboard?.playing?.contains(item.title) == true,
                            expanded: Binding(get: { expanded == item.id },
                                              set: { expanded = $0 ? item.id : nil }))
                }
            } header: {
                HStack {
                    Text("Clips")
                    Spacer()
                    Button {
                        daemon.fire("stop")
                    } label: {
                        Label("Stop all", systemImage: "stop.fill")
                    }
                    .controlSize(.small)
                }
            } footer: {
                Text("Only clips you restyle are written to config.json. The board lists those first, then the rest of the folder by name.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Section("Routing") {
                DevicePicker(title: "Microphone", selection: sb.input, options: daemon.devices?.inputs)
                DevicePicker(title: "Send mix to", selection: sb.output, options: daemon.devices?.outputs)
                Picker("Live on mic", selection: sb.live) {
                    Text("Auto").tag("auto")
                    Text("Always").tag("on")
                    Text("Off").tag("off")
                    if !["auto", "on", "off"].contains(store.config.soundboard.live) {
                        Text(store.config.soundboard.live).tag(store.config.soundboard.live)
                    }
                }
                .pickerStyle(.segmented)
                Text(liveHelp).font(.caption).foregroundColor(.secondary)
                Toggle("Mix my mic into the output", isOn: sb.passthrough)
                Toggle("Also play clips on my speakers", isOn: sb.monitor)
                DevicePicker(title: "Speakers", selection: sb.monitorDevice, options: daemon.devices?.outputs)
                    .disabled(!store.config.soundboard.monitor)
                LabeledContent("Clip volume") {
                    HStack {
                        Slider(value: sb.gain, in: 0...1)
                        Text("\(Int((store.config.soundboard.gain * 100).rounded()))%")
                            .monospacedDigit()
                            .frame(width: 44, alignment: .trailing)
                    }
                }
                HStack {
                    Spacer()
                    Button("Refresh devices") { Task { await daemon.loadDevices() } }
                        .disabled(daemon.reach != .running)
                }
            }

            Section {
                Label("In Zoom, Meet or Teams, choose BlackHole 2ch as your microphone. Your mic plays through quaked, mixed with the clips.",
                      systemImage: "info.circle")
            }
        }
        .formStyle(.grouped)
        .onAppear(perform: rescan)
        .onChange(of: store.config.soundboard) { _ in rescan() }
    }

    private var liveHelp: String {
        switch store.config.soundboard.live {
        case "on": return "Your mic is always routed through quaked."
        case "off": return "Your mic is never routed; only clips go out."
        default: return "Your mic is routed only while an app such as Zoom is listening to the output device."
        }
    }

    private func rescan() {
        items = ClipLibrary.items(store.config.soundboard)
    }

    /// Edits a clip's `clips` entry, adding it when it becomes customized and dropping it
    /// when it goes back to the defaults.
    private func clipBinding(_ item: ClipLibrary.Item) -> Binding<Clip> {
        let key = item.entryFile
        return Binding(
            get: { store.config.soundboard.clips.first { $0.file == key } ?? Clip(file: key) },
            set: { clip in
                var clips = store.config.soundboard.clips
                if let i = clips.firstIndex(where: { $0.file == key }) {
                    if clip.isCustomized { clips[i] = clip } else { clips.remove(at: i) }
                } else if clip.isCustomized {
                    clips.append(clip)
                }
                store.config.soundboard.clips = clips
            })
    }

    private func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.canCreateDirectories = true
        panel.prompt = "Choose"
        if folderExists { panel.directoryURL = URL(fileURLWithPath: ClipLibrary.expand(store.config.soundboard.folder)) }
        if panel.runModal() == .OK, let url = panel.url {
            store.config.soundboard.folder = (url.path as NSString).abbreviatingWithTildeInPath
        }
    }

    private func addClips() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = true
        panel.allowedContentTypes = [.audio]
        panel.prompt = "Add"
        guard panel.runModal() == .OK else { return }
        let folder = URL(fileURLWithPath: ClipLibrary.expand(store.config.soundboard.folder))
        var failed: [String] = []
        do {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        } catch {
            daemon.show("Couldn't create \(folder.path): \(error.localizedDescription)")
            return
        }
        for url in panel.urls {
            guard ClipLibrary.extensions.contains(url.pathExtension.lowercased()) else {
                failed.append(url.lastPathComponent)
                continue
            }
            if url.deletingLastPathComponent().standardizedFileURL == folder.standardizedFileURL { continue }
            do {
                try FileManager.default.copyItem(at: url, to: Self.freeName(for: url.lastPathComponent, in: folder))
            } catch {
                failed.append(url.lastPathComponent)
            }
        }
        if !failed.isEmpty {
            daemon.show("Not added: \(failed.joined(separator: ", ")) (use wav, mp3, m4a, aac, aiff, flac or caf).")
        }
        rescan()
    }

    /// `name` in `folder`, or "name 2.ext", "name 3.ext"… if it is taken.
    private static func freeName(for name: String, in folder: URL) -> URL {
        let stem = (name as NSString).deletingPathExtension
        let ext = (name as NSString).pathExtension
        var url = folder.appendingPathComponent(name)
        var n = 2
        while FileManager.default.fileExists(atPath: url.path) {
            url = folder.appendingPathComponent("\(stem) \(n)").appendingPathExtension(ext)
            n += 1
        }
        return url
    }

    private func reveal() {
        NSWorkspace.shared.open(URL(fileURLWithPath: ClipLibrary.expand(store.config.soundboard.folder)))
    }
}

private struct ClipRow: View {
    @EnvironmentObject var daemon: DaemonModel
    let item: ClipLibrary.Item
    @Binding var clip: Clip
    let playing: Bool
    @Binding var expanded: Bool

    var body: some View {
        DisclosureGroup(isExpanded: $expanded) {
            TextField("Title", text: $clip.title, prompt: Text(item.defaultTitle))
            SymbolField(title: "SF Symbol", name: $clip.symbol, placeholder: "waveform")
            HexColorField(title: "Color", hex: $clip.color, allowsDefault: true)
            LabeledContent("Volume") {
                HStack {
                    Slider(value: $clip.gain, in: 0...2)
                    Text("\(Int((clip.gain * 100).rounded()))%")
                        .monospacedDigit()
                        .frame(width: 44, alignment: .trailing)
                }
            }
            if clip.isCustomized {
                HStack {
                    Spacer()
                    Button("Reset to defaults") {
                        clip = Clip(file: clip.file)
                    }
                }
            }
        } label: {
            HStack(spacing: 10) {
                SymbolPreview(name: clip.symbol.isEmpty ? "waveform" : clip.symbol,
                              color: Color(hex: clip.color) ?? .accentColor)
                    .frame(width: 22)
                VStack(alignment: .leading, spacing: 1) {
                    Text(item.title)
                    Text(item.exists ? item.file : "\(item.file) (missing)")
                        .font(.caption)
                        .foregroundColor(item.exists ? .secondary : .orange)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer()
                if playing {
                    Image(systemName: "speaker.wave.2.fill").foregroundColor(.accentColor).help("Playing")
                }
                Button {
                    daemon.fire("play \(item.title)")
                } label: {
                    Label("Play", systemImage: "play.fill")
                }
                .controlSize(.small)
                .disabled(!item.exists)
            }
        }
    }
}
