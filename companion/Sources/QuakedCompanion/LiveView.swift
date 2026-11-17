// What the Quake shows right now, clickable, with the knob and page controls under it.

import AppKit
import SwiftUI

struct LiveView: View {
    @EnvironmentObject var daemon: DaemonModel
    @State private var frame: NSImage?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                screen
                controls
                StatusGrid(status: daemon.status, reach: daemon.reach)
            }
            .padding(20)
        }
        .task { await poll() }
    }

    // MARK: Screen

    private var screen: some View {
        ZStack {
            Color.black
            if let frame {
                Image(nsImage: frame).resizable().interpolation(.high)
            } else {
                Text(placeholder)
                    .font(.callout)
                    .foregroundColor(.white.opacity(0.6))
                    .multilineTextAlignment(.center)
                    .padding()
            }
        }
        .aspectRatio(4, contentMode: .fit)
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Color.secondary.opacity(0.4)))
        .overlay(
            GeometryReader { geo in
                Color.clear
                    .contentShape(Rectangle())
                    .gesture(SpatialTapGesture().onEnded { tap in
                        guard frame != nil, geo.size.width > 0, geo.size.height > 0 else { return }
                        let x = Int((tap.location.x / geo.size.width * 1920).rounded())
                        let y = Int((tap.location.y / geo.size.height * 480).rounded())
                        send("tap \(min(max(x, 0), 1919)) \(min(max(y, 0), 479))")
                    })
            }
        )
        .help("Click to tap the Quake's screen")
    }

    private var placeholder: String {
        switch daemon.reach {
        case .notRunning: return "quaked isn't running"
        case .outdated: return "This quaked build can't send its screen yet"
        case .unknown, .running: return "Waiting for the screen…"
        }
    }

    // MARK: Controls

    private var controls: some View {
        HStack(alignment: .top, spacing: 28) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Knob").font(.headline)
                HStack {
                    knob("Turn left", "arrow.counterclockwise", "turn -1", key: .leftArrow)
                    knob("Press", "circle.inset.filled", "press")
                    knob("Turn right", "arrow.clockwise", "turn 1", key: .rightArrow)
                    knob("Back", "chevron.backward", "double", hint: "Double-click")
                    knob("Home", "house", "hold", hint: "Hold")
                }
            }
            VStack(alignment: .leading, spacing: 6) {
                Text("Page").font(.headline)
                HStack {
                    ForEach(PageId.allCases) { page in
                        if daemon.status?.page == page.rawValue {
                            Button(page.label) { send("page \(page.rawValue)") }.buttonStyle(.borderedProminent)
                        } else {
                            Button(page.label) { send("page \(page.rawValue)") }.buttonStyle(.bordered)
                        }
                    }
                }
            }
            Spacer(minLength: 0)
        }
        .controlSize(.large)
    }

    private func knob(_ title: String, _ symbol: String, _ command: String,
                      key: KeyEquivalent? = nil, hint: String? = nil) -> some View {
        let button = Button { send(command) } label: { Label(title, systemImage: symbol) }
            .help(hint.map { "\(title) (\($0) on the knob)" } ?? title)
        return Group {
            if let key {
                button.keyboardShortcut(key, modifiers: [])
            } else {
                button
            }
        }
    }

    // MARK: Polling

    private func send(_ command: String) {
        Task {
            await daemon.command(command)
            await refreshFrame()
        }
    }

    private func refreshFrame() async {
        guard daemon.reach == .running else { frame = nil; return }
        if let data = await daemon.frame(), let image = NSImage(data: data) { frame = image }
    }

    /// ~3 frames a second while this view is on screen; `.task` cancels it when it goes away.
    private func poll() async {
        var tick = 0
        while !Task.isCancelled {
            if NSApp.occlusionState.contains(.visible) {
                if tick % 3 == 0 { await daemon.refreshStatus() }
                await refreshFrame()
                tick += 1
            }
            try? await Task.sleep(nanoseconds: 333_000_000)
        }
    }
}

private struct StatusGrid: View {
    let status: DaemonStatus?
    let reach: Reachability

    var body: some View {
        GroupBox {
            if let s = status {
                Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 6) {
                    item("Device", s.connected == true ? "Connected" : "Not connected",
                         tint: s.connected == true ? .green : .orange)
                    item("Page", pageText(s))
                    item("Sessions", sessionsText(s))
                    item("Now playing", trackText(s.track))
                    item("Soundboard", soundboardText(s.soundboard), tint: soundboardTint(s.soundboard))
                    if let playing = s.soundboard?.playing, !playing.isEmpty {
                        item("Clips playing", playing.joined(separator: ", "))
                    }
                    if s.soundboard?.micMuted == true { item("Mic", "Muted", tint: .red) }
                    if let v = s.version { item("quaked", v) }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                Text(reach == .notRunning ? "No status: quaked isn't running." : "No status yet.")
                    .foregroundColor(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        } label: {
            Text("Status")
        }
    }

    private func item(_ label: String, _ value: String, tint: Color? = nil) -> some View {
        GridRow {
            Text(label).foregroundColor(.secondary)
            HStack(spacing: 6) {
                if let tint { Circle().fill(tint).frame(width: 8, height: 8) }
                Text(value).textSelection(.enabled)
            }
        }
    }

    private func pageText(_ s: DaemonStatus) -> String {
        let page = s.page.flatMap(PageId.init(rawValue:))?.label ?? s.page ?? "?"
        switch s.level {
        case "focus": return "\(page), focused"
        case "detail": return "\(page), detail"
        default: return page
        }
    }

    private func sessionsText(_ s: DaemonStatus) -> String {
        let n = s.sessions ?? 0
        guard n > 0 else { return "None" }
        return "\(n) (\(s.waiting ?? 0) waiting on you, \(s.working ?? 0) working)"
    }

    private func trackText(_ t: DaemonStatus.Track?) -> String {
        guard let t, let title = t.title, !title.isEmpty else { return "Nothing" }
        let artist = (t.artist ?? "").isEmpty ? "" : " by \(t.artist!)"
        return "\(title)\(artist)\(t.playing == true ? "" : " (paused)")"
    }

    private func soundboardText(_ sb: DaemonStatus.SoundboardState?) -> String {
        guard let sb else { return "Off" }
        if let error = sb.error, !error.isEmpty { return error }
        if sb.routing == true {
            if let listener = sb.listener, !listener.isEmpty { return "Live — \(listener)" }
            return "Live"
        }
        return "Idle (mic not routed)"
    }

    private func soundboardTint(_ sb: DaemonStatus.SoundboardState?) -> Color? {
        guard let sb else { return nil }
        if let error = sb.error, !error.isEmpty { return .red }
        return sb.routing == true ? .green : .secondary
    }
}
