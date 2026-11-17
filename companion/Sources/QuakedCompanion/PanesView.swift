// Panes: which pages the board shows, and in what order.

import SwiftUI

extension PageId {
    /// Same SF Symbol the board uses for the page.
    var symbol: String {
        switch self {
        case .dash: return "square.grid.2x2.fill"
        case .ai: return "sparkles"
        case .music: return "music.note"
        case .shortcuts: return "square.grid.3x3.fill"
        case .soundboard: return "waveform"
        case .league: return "gamecontroller.fill"
        case .docker: return "shippingbox.fill"
        case .ops: return "server.rack"
        case .code: return "arrow.triangle.pull"
        case .settings: return "gearshape.fill"
        }
    }

    var blurb: String {
        switch self {
        case .dash: return "A tile per pane with its status at a glance"
        case .ai: return "Claude Code sessions, their steps, who needs you"
        case .music: return "Spotify or Music: now playing, controls, volume"
        case .shortcuts: return "Tiles that type keys, open things, run Shortcuts"
        case .soundboard: return "Clips into your call through the virtual mic"
        case .league: return "League client: queue, auto-accept, champ select"
        case .docker: return "Containers on Kiri or Docker, with logs"
        case .ops: return "WARP, your servers and your sites"
        case .code: return "GitHub PRs and GitLab merge requests"
        case .settings: return "Brightness and switches, right on the board"
        }
    }
}

struct PanesView: View {
    @EnvironmentObject var store: ConfigStore
    /// Every pane in display order, the ones that are on first; ones turned off keep
    /// their place here while the window is open.
    @State private var order: [PageId] = []

    private var shown: [PageId] { store.config.pages }

    var body: some View {
        Form {
            Section {
                ForEach(Array(order.enumerated()), id: \.element) { index, page in
                    HStack(spacing: 12) {
                        Image(systemName: page.symbol)
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundColor(Color(hex: store.config.ring.color(for: page)) ?? .accentColor)
                            .frame(width: 24)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(page.label).font(.body.weight(.medium))
                            Text(page.blurb).font(.caption).foregroundColor(.secondary)
                        }
                        Spacer()
                        Button { move(index, by: -1) } label: { Image(systemName: "chevron.up") }
                            .disabled(index == 0)
                            .help("Move up")
                        Button { move(index, by: 1) } label: { Image(systemName: "chevron.down") }
                            .disabled(index == order.count - 1)
                            .help("Move down")
                        Toggle(page.label, isOn: included(page))
                            .toggleStyle(.switch)
                            .labelsHidden()
                            .disabled(shown == [page])
                    }
                    .buttonStyle(.borderless)
                    .opacity(shown.contains(page) ? 1 : 0.55)
                    .padding(.vertical, 2)
                }
                .onMove { order.move(fromOffsets: $0, toOffset: $1); commit() }
            } header: {
                Text("\(shown.count) of \(PageId.allCases.count) panes on")
            } footer: {
                Text("A pane that's off disappears from the board and the dashboard. The knob steps through the panes that are on, in this order, and the board starts on the first one. At least one stays on.")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }

            Section {
                HStack {
                    Button("Turn all on") { store.config.pages = order }
                        .disabled(shown.count == PageId.allCases.count)
                    Button("Default order") {
                        let all = PageId.allCases
                        store.config.pages = all.filter(shown.contains)
                        order = store.config.pages + all.filter { !shown.contains($0) }
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
