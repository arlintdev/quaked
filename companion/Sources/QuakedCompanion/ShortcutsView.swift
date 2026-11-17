// The Shortcuts page's tiles: a reorderable list and an editor for the selected tile.

import AppKit
import SwiftUI

struct ShortcutsView: View {
    @EnvironmentObject var store: ConfigStore
    @State private var selection: UUID?

    private var tiles: [Shortcut] { store.config.shortcuts }

    var body: some View {
        HStack(spacing: 0) {
            VStack(spacing: 0) {
                List(selection: $selection) {
                    ForEach(tiles) { tile in
                        TileRow(tile: tile).tag(tile.id)
                            .contextMenu { Button("Delete") { delete(tile.id) } }
                    }
                    .onMove { store.config.shortcuts.move(fromOffsets: $0, toOffset: $1) }
                }
                .onDeleteCommand { if let id = selection { delete(id) } }
                Divider()
                HStack(spacing: 4) {
                    Button(action: add) { Image(systemName: "plus") }
                        .disabled(tiles.count >= Config.maxShortcuts)
                        .help("Add a tile")
                    Button { if let id = selection { delete(id) } } label: { Image(systemName: "minus") }
                        .disabled(selection == nil)
                        .help("Delete the selected tile")
                    Spacer()
                    Text("\(tiles.count) of \(Config.maxShortcuts)").font(.caption).foregroundColor(.secondary)
                }
                .buttonStyle(.borderless)
                .padding(8)
            }
            .frame(minWidth: 230, idealWidth: 260, maxWidth: 320)

            Divider()

            if let index = tiles.firstIndex(where: { $0.id == selection }) {
                TileEditor(tile: $store.config.shortcuts[index], index: index)
                    .id(tiles[index].id)
            } else {
                VStack(spacing: 8) {
                    Image(systemName: "square.grid.2x2").font(.largeTitle).foregroundColor(.secondary)
                    Text(tiles.isEmpty ? "No tiles yet. Add one with +." : "Select a tile to edit it.")
                        .foregroundColor(.secondary)
                    Text("Drag tiles to change their order on the board.").font(.caption).foregroundColor(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .onAppear { if selection == nil { selection = tiles.first?.id } }
    }

    private func add() {
        guard tiles.count < Config.maxShortcuts else { return }
        let tile = Shortcut(title: "New shortcut", symbol: "star.fill", color: "#4C8DFF", action: Action(.keys))
        if let i = tiles.firstIndex(where: { $0.id == selection }) {
            store.config.shortcuts.insert(tile, at: i + 1)
        } else {
            store.config.shortcuts.append(tile)
        }
        selection = tile.id
    }

    private func delete(_ id: UUID) {
        guard let i = tiles.firstIndex(where: { $0.id == id }) else { return }
        store.config.shortcuts.remove(at: i)
        if selection == id {
            selection = tiles.isEmpty ? nil : tiles[min(i, tiles.count - 1)].id
        }
    }
}

private struct TileRow: View {
    let tile: Shortcut

    var body: some View {
        HStack(spacing: 10) {
            SymbolPreview(name: tile.symbol, color: Color(hex: tile.color) ?? .secondary)
                .font(.title3)
                .frame(width: 26)
            VStack(alignment: .leading, spacing: 2) {
                Text(tile.title.isEmpty ? "Untitled" : tile.title)
                Text(tile.action.summary).font(.caption).foregroundColor(.secondary).lineLimit(1)
            }
        }
        .padding(.vertical, 2)
    }
}

private struct TileEditor: View {
    @EnvironmentObject var store: ConfigStore
    @EnvironmentObject var daemon: DaemonModel
    @Binding var tile: Shortcut
    let index: Int

    var body: some View {
        Form {
            Section {
                HStack {
                    Spacer()
                    TilePreview(title: tile.title, symbol: tile.symbol, hex: tile.color)
                    Spacer()
                }
                TextField("Title", text: $tile.title)
                SymbolField(title: "SF Symbol", name: $tile.symbol)
                HexColorField(title: "Color", hex: $tile.color, allowsDefault: true)
            }

            Section("Action") {
                Picker("Type", selection: kind) {
                    ForEach(ActionKind.allCases) { Text($0.label).tag(Optional($0)) }
                    if tile.action.kind == nil {
                        Text("Unknown (\(tile.action.type))").tag(ActionKind?.none)
                    }
                }
                actionFields
            }

            Section {
                HStack {
                    Button {
                        Task {
                            await store.saveNow()
                            await daemon.command("shortcut \(index)")
                        }
                    } label: {
                        Label("Run", systemImage: "play.fill")
                    }
                    Text("Runs tile \(index + 1) on the Mac, as if you tapped it.")
                        .font(.caption)
                        .foregroundColor(.secondary)
                }
            }
        }
        .formStyle(.grouped)
    }

    private var kind: Binding<ActionKind?> {
        Binding(get: { tile.action.kind },
                set: { new in
                    if let new, new != tile.action.kind { tile.action = Action(new) }
                })
    }

    private var value: Binding<String> {
        Binding(get: { tile.action.value }, set: { tile.action.value = $0 })
    }

    @ViewBuilder private var actionFields: some View {
        switch tile.action.kind {
        case .keys:
            TextField("Keys", text: value, prompt: Text("cmd+shift+4"))
                .font(.body.monospaced())
            if let problem = KeyCombo.problem(tile.action.value) {
                Label(problem, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundColor(.orange)
            }
            Text("Typed by the Quake as a USB keyboard. Join modifiers (cmd, ctrl, alt or opt, shift) with “+” and end with one key: a–z, 0–9, f1–f24, up, down, left, right, space, return, tab, escape or delete. Examples: cmd+shift+4, ctrl+up, f5.")
                .font(.caption)
                .foregroundColor(.secondary)
        case .open:
            LabeledContent("Open") {
                HStack {
                    TextField("Open", text: value, prompt: Text("/Applications/Safari.app or https://…"))
                        .labelsHidden()
                    Button("Choose…", action: chooseTarget)
                }
            }
            Text("An app, file, folder or URL, opened like `open` does.")
                .font(.caption)
                .foregroundColor(.secondary)
        case .shortcut:
            TextField("Shortcut name", text: value, prompt: Text("Name in the Shortcuts app"))
        case .shell:
            TextField("Command", text: value, prompt: Text("say hello"), axis: .vertical)
                .font(.body.monospaced())
                .lineLimit(1...5)
            Text("Run with /bin/sh -c.").font(.caption).foregroundColor(.secondary)
        case .micMute:
            Text("Mutes or unmutes your mic in the soundboard mix.").foregroundColor(.secondary)
        case .media:
            Picker("Command", selection: value) {
                Text("Play / pause").tag("playpause")
                Text("Next track").tag("next")
                Text("Previous track").tag("previous")
            }
            Text("Controls Spotify or Music.").font(.caption).foregroundColor(.secondary)
        case .clip:
            LabeledContent("Clip title") {
                HStack {
                    TextField("Clip title", text: value).labelsHidden()
                    Menu("Clips") {
                        let items = ClipLibrary.items(store.config.soundboard)
                        if items.isEmpty { Text("No clips in the soundboard folder") }
                        ForEach(items) { item in Button(item.title) { value.wrappedValue = item.title } }
                    }
                    .fixedSize()
                }
            }
        case .page:
            Picker("Page", selection: value) {
                ForEach(PageId.allCases) { Text($0.label).tag($0.rawValue) }
            }
        case nil:
            Text("This action type isn't known to this app; it is saved unchanged.")
                .foregroundColor(.secondary)
        }
    }

    private func chooseTarget() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = true
        panel.treatsFilePackagesAsDirectories = false
        panel.directoryURL = URL(fileURLWithPath: "/Applications")
        if panel.runModal() == .OK, let url = panel.url { tile.action.value = url.path }
    }
}
