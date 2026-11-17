// Loads and saves config.json. Edits are saved half a second after they stop, then the
// daemon is told to `reload`. Keys this app doesn't know are kept as they were.

import AppKit

@MainActor
final class ConfigStore: ObservableObject {
    @Published var config = Config() {
        didSet { if !loading && config != oldValue { scheduleSave() } }
    }
    /// A load or save problem worth showing.
    @Published private(set) var problem: String?

    static let url = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Application Support/Quaked/config.json")

    weak var daemon: DaemonModel?
    private var raw: [String: Any] = [:]
    private var loading = false
    private var saveTask: Task<Void, Never>?
    private var fileDate: Date?
    /// The file on disk didn't parse; keep a copy before the first save replaces it.
    private var keepInvalidCopy = false

    init() {
        load()
        NotificationCenter.default.addObserver(forName: NSApplication.didBecomeActiveNotification,
                                               object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.reloadIfChangedOnDisk() }
        }
    }

    var fileExists: Bool { FileManager.default.fileExists(atPath: Self.url.path) }

    func load() {
        let url = Self.url
        var next = Config()
        raw = [:]
        keepInvalidCopy = false
        problem = nil
        if let data = try? Data(contentsOf: url) {
            do {
                next = try JSONDecoder().decode(Config.self, from: data)
                raw = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
            } catch {
                keepInvalidCopy = true
                problem = "config.json isn't valid JSON, so the defaults are shown. Your first edit replaces it (the old file is kept as config.invalid.json)."
            }
        }
        fileDate = Self.modified()
        loading = true
        if next != config { config = next }
        loading = false
    }

    /// Picks up edits made elsewhere (by hand, or the daemon creating the file).
    func reloadIfChangedOnDisk() {
        guard saveTask == nil, Self.modified() != fileDate else { return }
        load()
    }

    private func scheduleSave() {
        saveTask?.cancel()
        saveTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 500_000_000)
            guard !Task.isCancelled else { return }
            await self?.saveNow()
        }
    }

    /// Writes pending edits now and has the daemon reload them.
    func saveNow() async {
        saveTask?.cancel()
        saveTask = nil
        if write() { await daemon?.command("reload", quietIfDown: true) }
    }

    /// Synchronous flush for quitting.
    func flush() {
        guard saveTask != nil else { return }
        saveTask?.cancel()
        saveTask = nil
        if write() { _ = daemon?.ctl.request("reload", timeout: 1) }
    }

    @discardableResult
    private func write() -> Bool {
        let url = Self.url
        do {
            let (data, merged) = try config.fileData(keeping: raw)
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            if keepInvalidCopy {
                let copy = url.deletingLastPathComponent().appendingPathComponent("config.invalid.json")
                try? FileManager.default.removeItem(at: copy)
                try? FileManager.default.copyItem(at: url, to: copy)
                keepInvalidCopy = false
            }
            try data.write(to: url, options: .atomic)
            raw = merged
            fileDate = Self.modified()
            problem = nil
            return true
        } catch {
            problem = "Couldn't save config.json: \(error.localizedDescription)"
            return false
        }
    }

    private static func modified() -> Date? {
        (try? FileManager.default.attributesOfItem(atPath: url.path))?[.modificationDate] as? Date
    }
}
