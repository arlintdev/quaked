// Quaked Companion: settings and a live mirror for the quaked daemon.

import AppKit
import SwiftUI

@main
struct QuakedCompanionApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        Window("Quaked Companion", id: "main") {
            ContentView()
                .environmentObject(delegate.store)
                .environmentObject(delegate.daemon)
                .frame(minWidth: 820, minHeight: 540)
        }
        .defaultSize(width: 1000, height: 700)
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let store = ConfigStore()
    let daemon = DaemonModel()

    override init() {
        super.init()
        store.daemon = daemon
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    func applicationWillTerminate(_ notification: Notification) { store.flush() }
}

enum SidebarItem: String, CaseIterable, Identifiable {
    case live, shortcuts, soundboard, lighting, general

    var id: String { rawValue }

    var label: String {
        switch self {
        case .live: return "Live"
        case .shortcuts: return "Shortcuts"
        case .soundboard: return "Soundboard"
        case .lighting: return "Lighting"
        case .general: return "General"
        }
    }

    var symbol: String {
        switch self {
        case .live: return "display"
        case .shortcuts: return "square.grid.2x2"
        case .soundboard: return "waveform"
        case .lighting: return "light.max"
        case .general: return "gearshape"
        }
    }
}

struct ContentView: View {
    @SceneStorage("sidebar") private var stored = SidebarItem.live.rawValue

    private var selection: Binding<SidebarItem?> {
        Binding(get: { SidebarItem(rawValue: stored) ?? .live },
                set: { if let item = $0 { stored = item.rawValue } })
    }

    var body: some View {
        NavigationSplitView {
            List(selection: selection) {
                ForEach(SidebarItem.allCases) { item in
                    Label(item.label, systemImage: item.symbol).tag(item)
                }
            }
            .navigationSplitViewColumnWidth(min: 160, ideal: 180)
        } detail: {
            let item = selection.wrappedValue ?? .live
            VStack(spacing: 0) {
                DaemonBanner()
                Group {
                    switch item {
                    case .live: LiveView()
                    case .shortcuts: ShortcutsView()
                    case .soundboard: SoundboardView()
                    case .lighting: LightingView()
                    case .general: GeneralView()
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            .navigationTitle(item.label)
        }
    }
}
