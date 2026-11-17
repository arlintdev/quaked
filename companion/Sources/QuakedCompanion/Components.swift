// Small views shared by the pages: hex colors, SF Symbol fields, device pickers, the banner.

import AppKit
import SwiftUI

extension Color {
    /// "#RRGGBB" -> Color, nil when it isn't six hex digits.
    init?(hex: String) {
        let digits = hex.trimmingCharacters(in: .whitespaces).replacingOccurrences(of: "#", with: "")
        guard digits.count == 6, let v = UInt32(digits, radix: 16) else { return nil }
        self.init(.sRGB, red: Double((v >> 16) & 255) / 255, green: Double((v >> 8) & 255) / 255,
                  blue: Double(v & 255) / 255)
    }

    var hexString: String? {
        guard let c = NSColor(self).usingColorSpace(.sRGB) else { return nil }
        func byte(_ x: CGFloat) -> Int { Int((min(max(x, 0), 1) * 255).rounded()) }
        return String(format: "#%02X%02X%02X", byte(c.redComponent), byte(c.greenComponent), byte(c.blueComponent))
    }
}

func symbolExists(_ name: String) -> Bool {
    !name.isEmpty && NSImage(systemSymbolName: name, accessibilityDescription: nil) != nil
}

/// The symbol, or a warning glyph when the name isn't an SF Symbol.
struct SymbolPreview: View {
    let name: String
    var color: Color = .primary

    var body: some View {
        if symbolExists(name) {
            Image(systemName: name).foregroundColor(color)
        } else {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundColor(.yellow)
                .help(name.isEmpty ? "No symbol" : "“\(name)” isn't an SF Symbol")
        }
    }
}

struct SymbolField: View {
    let title: String
    @Binding var name: String
    /// Shown (and previewed) while `name` is empty.
    var placeholder = ""

    var body: some View {
        LabeledContent(title) {
            HStack {
                TextField(title, text: $name, prompt: Text(placeholder.isEmpty ? "e.g. star.fill" : placeholder))
                    .labelsHidden()
                SymbolPreview(name: name.isEmpty ? placeholder : name)
                    .font(.title3)
                    .frame(width: 26)
            }
        }
    }
}

/// A "#RRGGBB" string edited as text or with the system color picker.
struct HexColorField: View {
    let title: String
    @Binding var hex: String
    /// Allow "" (the daemon's default color) and offer a button to go back to it.
    var allowsDefault = false

    var body: some View {
        LabeledContent(title) {
            HStack(spacing: 8) {
                if !hex.isEmpty && Color(hex: hex) == nil {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundColor(.yellow)
                        .help("Use #RRGGBB")
                }
                TextField(title, text: $hex, prompt: Text(allowsDefault ? "Default" : "#RRGGBB"))
                    .labelsHidden()
                    .font(.body.monospaced())
                    .frame(width: 90)
                ColorPicker(title, selection: color, supportsOpacity: false)
                    .labelsHidden()
                if allowsDefault && !hex.isEmpty {
                    Button { hex = "" } label: { Image(systemName: "xmark.circle.fill") }
                        .buttonStyle(.borderless)
                        .foregroundColor(.secondary)
                        .help("Use the default color")
                }
            }
        }
    }

    private var color: Binding<Color> {
        Binding(get: { Color(hex: hex) ?? Color(nsColor: .tertiaryLabelColor) },
                set: { if let h = $0.hexString { hex = h } })
    }
}

/// A device name picked from the daemon's list ("" = system default), or typed when the
/// daemon can't list devices.
struct DevicePicker: View {
    let title: String
    @Binding var selection: String
    let options: [String]?

    var body: some View {
        if let options {
            Picker(title, selection: $selection) {
                Text("System default").tag("")
                ForEach(options, id: \.self) { Text($0).tag($0) }
                if !selection.isEmpty && !options.contains(selection) {
                    Text("\(selection) (not connected)").tag(selection)
                }
            }
        } else {
            TextField(title, text: $selection, prompt: Text("System default"))
        }
    }
}

/// Daemon and config problems, shown above every page.
struct DaemonBanner: View {
    @EnvironmentObject var daemon: DaemonModel
    @EnvironmentObject var store: ConfigStore

    var body: some View {
        VStack(spacing: 0) {
            switch daemon.reach {
            case .notRunning:
                row("bolt.horizontal.circle", .orange,
                    "quaked isn't running. Edits are still saved to config.json and apply when it starts.")
            case .outdated:
                row("arrow.triangle.2.circlepath", .orange,
                    "This quaked build doesn't answer status requests. Reinstall it (./install.sh) for the live view, devices and playback.")
            case .unknown, .running:
                EmptyView()
            }
            if let problem = store.problem {
                row("exclamationmark.triangle.fill", .red, problem)
            }
            if let message = daemon.message {
                row("xmark.octagon.fill", .red, message) { daemon.message = nil }
            }
        }
    }

    private func row(_ symbol: String, _ tint: Color, _ text: String, dismiss: (() -> Void)? = nil) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: symbol).foregroundColor(tint)
            Text(text).frame(maxWidth: .infinity, alignment: .leading)
            if let dismiss {
                Button(action: dismiss) { Image(systemName: "xmark") }
                    .buttonStyle(.borderless)
            }
        }
        .font(.callout)
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(tint.opacity(0.12))
    }
}

/// A rough picture of a tile as the board draws it.
struct TilePreview: View {
    let title: String
    let symbol: String
    let hex: String

    var body: some View {
        let tint = Color(hex: hex) ?? .gray
        VStack(spacing: 6) {
            SymbolPreview(name: symbol, color: tint).font(.system(size: 26, weight: .semibold))
            Text(title.isEmpty ? "Untitled" : title)
                .font(.caption.weight(.semibold))
                .lineLimit(1)
                .foregroundColor(.white)
        }
        .frame(width: 120, height: 84)
        .background(RoundedRectangle(cornerRadius: 12).fill(Color.black.opacity(0.85)))
        .overlay(RoundedRectangle(cornerRadius: 12).stroke(tint.opacity(0.8), lineWidth: 2))
    }
}
