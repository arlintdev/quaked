// Mirrors ../src/config.rs: ~/Library/Application Support/Quaked/config.json, camelCase.
// Every field falls back to the daemon's default when it is missing or malformed.

import Foundation

enum PageId: String, Codable, CaseIterable, Identifiable {
    case ai, music, shortcuts, soundboard, league, docker, ops, code, settings

    var id: String { rawValue }

    var label: String {
        switch self {
        case .ai: return "AI"
        case .music: return "Music"
        case .shortcuts: return "Shortcuts"
        case .soundboard: return "Sounds"
        case .league: return "League"
        case .docker: return "Docker"
        case .ops: return "Ops"
        case .code: return "Code"
        case .settings: return "Settings"
        }
    }
}

struct Config: Codable, Equatable {
    var version = 1
    /// Chirp when a Claude session starts waiting on you.
    var buzzer = true
    /// Seconds without input before the board returns to the top level.
    var idleSeconds = 45
    /// Pages in knob order; a page left out is hidden.
    var pages: [PageId] = PageId.allCases
    var ring = Ring()
    var touch = Touch()
    var shortcuts: [Shortcut] = Shortcut.defaults
    var soundboard = Soundboard()
    var league = League()
    var docker = Docker()
    var ops = Ops()
    var code = Code()
    var alerts = Alerts()
    var sync = SyncSettings()
    var device = DeviceSettings()

    static let maxShortcuts = 16

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let d = Config()
        version = c.value(.version, d.version)
        buzzer = c.value(.buzzer, d.buzzer)
        idleSeconds = c.value(.idleSeconds, d.idleSeconds)
        let pageNames: [String] = c.value(.pages, d.pages.map(\.rawValue))
        var seen = Set<PageId>()
        let parsed = pageNames.compactMap(PageId.init(rawValue:)).filter { seen.insert($0).inserted }
        pages = parsed.isEmpty ? d.pages : parsed
        ring = c.value(.ring, d.ring)
        touch = c.value(.touch, d.touch)
        shortcuts = c.value(.shortcuts, d.shortcuts)
        soundboard = c.value(.soundboard, d.soundboard)
        league = c.value(.league, d.league)
        docker = c.value(.docker, d.docker)
        ops = c.value(.ops, d.ops)
        code = c.value(.code, d.code)
        alerts = c.value(.alerts, d.alerts)
        sync = c.value(.sync, d.sync)
        device = c.value(.device, d.device)
    }
}

/// League of Legends client: auto-accept and jumping to the page when the queue pops.
struct League: Codable, Equatable {
    var autoAccept = true
    var acceptDelayMs = 1500
    var snap = true

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let d = League()
        autoAccept = c.value(.autoAccept, d.autoAccept)
        acceptDelayMs = c.value(.acceptDelayMs, d.acceptDelayMs)
        snap = c.value(.snap, d.snap)
    }
}

/// Docker-compatible engine socket; "" = find one (Kiri's first).
struct Docker: Codable, Equatable {
    var socket = ""

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        socket = c.value(.socket, "")
    }
}

/// OPS page: SSH host aliases, whether to read their load / memory / disk / GPU, and sites.
struct Ops: Codable, Equatable {
    var hosts = ["workbench", "shebash-gpu", "spark.d3d.dev", "pi"]
    var sshStats = true
    var sites = ["https://arlint.dev", "https://digin.work", "https://ibtg.notdone.dev", "https://plcplayground.notdone.dev"]

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let d = Ops()
        hosts = c.value(.hosts, d.hosts)
        sshStats = c.value(.sshStats, d.sshStats)
        sites = c.value(.sites, d.sites)
    }
}

/// CODE page: GitHub pull requests through the gh CLI.
struct Code: Codable, Equatable {
    var refreshSeconds = 60
    /// Include GitLab merge requests (through glab).
    var gitlab = true
    /// Empty = every host in glab's config.
    var gitlabHosts: [String] = []

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        refreshSeconds = c.value(.refreshSeconds, 60)
        gitlab = c.value(.gitlab, true)
        gitlabHosts = c.value(.gitlabHosts, [])
    }
}

/// Blink the ring and jump to the page when something new shows up.
struct Alerts: Codable, Equatable {
    var blink = true
    var jump = true
    var seconds = 300
    var ai = true
    var code = true
    var ops = true
    var docker = true

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let d = Alerts()
        blink = c.value(.blink, d.blink)
        jump = c.value(.jump, d.jump)
        seconds = c.value(.seconds, d.seconds)
        ai = c.value(.ai, d.ai)
        code = c.value(.code, d.code)
        ops = c.value(.ops, d.ops)
        docker = c.value(.docker, d.docker)
    }
}

/// Bonjour sync between your Macs; both need the same key.
struct SyncSettings: Codable, Equatable {
    var enabled = true
    var key = ""
    /// Empty = this Mac's computer name.
    var name = ""

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        enabled = c.value(.enabled, true)
        key = c.value(.key, "")
        name = c.value(.name, "")
    }
}

/// Hardware settings applied whenever the Quake connects.
struct DeviceSettings: Codable, Equatable {
    /// The Quake's own microphone; off also turns off its indicator light.
    var mic = false
    /// Screen luminance (10-255) applied on connect; nil leaves the device's own.
    var screenBrightness: Int?

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        mic = c.value(.mic, false)
        screenBrightness = try? c.decodeIfPresent(Int.self, forKey: .screenBrightness)
    }
}

struct Ring: Codable, Equatable {
    /// Drive the knob's light ring (off = leave it as the device has it).
    var enabled = true
    var brightness = 200
    var ai = "#D97757"
    var music = "#1DB954"
    var shortcuts = "#4C8DFF"
    var soundboard = "#B36BFF"
    var league = "#C89B3C"
    var docker = "#2496ED"
    var ops = "#7FD1B9"
    var code = "#A371F7"
    var settings = "#D0D0D8"
    /// Breathe in `alertColor` while a Claude session needs you.
    var alert = true
    var alertColor = "#F2B705"

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let d = Ring()
        enabled = c.value(.enabled, d.enabled)
        brightness = c.value(.brightness, d.brightness)
        ai = c.value(.ai, d.ai)
        music = c.value(.music, d.music)
        shortcuts = c.value(.shortcuts, d.shortcuts)
        soundboard = c.value(.soundboard, d.soundboard)
        league = c.value(.league, d.league)
        docker = c.value(.docker, d.docker)
        ops = c.value(.ops, d.ops)
        code = c.value(.code, d.code)
        settings = c.value(.settings, d.settings)
        alert = c.value(.alert, d.alert)
        alertColor = c.value(.alertColor, d.alertColor)
    }

    func color(for page: PageId) -> String {
        switch page {
        case .ai: return ai
        case .music: return music
        case .shortcuts: return shortcuts
        case .soundboard: return soundboard
        case .league: return league
        case .docker: return docker
        case .ops: return ops
        case .code: return code
        case .settings: return settings
        }
    }

    static func keyPath(for page: PageId) -> WritableKeyPath<Ring, String> {
        switch page {
        case .ai: return \.ai
        case .music: return \.music
        case .shortcuts: return \.shortcuts
        case .soundboard: return \.soundboard
        case .league: return \.league
        case .docker: return \.docker
        case .ops: return \.ops
        case .code: return \.code
        case .settings: return \.settings
        }
    }
}

/// How touch-panel coordinates map onto the picture (the panel reports y from the bottom).
struct Touch: Codable, Equatable {
    var flipX = false
    var flipY = true

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let d = Touch()
        flipX = c.value(.flipX, d.flipX)
        flipY = c.value(.flipY, d.flipY)
    }
}

struct Shortcut: Codable, Equatable, Identifiable {
    /// Only for SwiftUI lists; not saved.
    var id = UUID()
    var title = ""
    /// SF Symbol name, e.g. "camera.viewfinder".
    var symbol = ""
    var color = ""
    var action = Action(.keys)

    enum CodingKeys: String, CodingKey { case title, symbol, color, action }

    init(title: String, symbol: String, color: String, action: Action) {
        self.title = title
        self.symbol = symbol
        self.color = color
        self.action = action
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        title = c.value(.title, "")
        symbol = c.value(.symbol, "")
        color = c.value(.color, "")
        action = c.value(.action, Action(.keys))
    }

    static func == (a: Shortcut, b: Shortcut) -> Bool {
        a.title == b.title && a.symbol == b.symbol && a.color == b.color && a.action == b.action
    }

    static var defaults: [Shortcut] {
        [
            Shortcut(title: "Mute mic", symbol: "mic.slash.fill", color: "#FF6B6B", action: Action(.micMute)),
            Shortcut(title: "Screenshot", symbol: "camera.viewfinder", color: "#8AB4F8", action: Action(.keys, "cmd+shift+4")),
            Shortcut(title: "Lock screen", symbol: "lock.fill", color: "#C9C9D1", action: Action(.keys, "ctrl+cmd+q")),
            Shortcut(title: "Mission Control", symbol: "rectangle.3.group.fill", color: "#7FD1B9", action: Action(.keys, "ctrl+up")),
            Shortcut(title: "Claude", symbol: "sparkles", color: "#D97757", action: Action(.open, "claude://")),
            Shortcut(title: "Terminal", symbol: "terminal.fill", color: "#C9C9D1", action: Action(.open, "/System/Applications/Utilities/Terminal.app")),
            Shortcut(title: "Spotify", symbol: "music.note", color: "#1DB954", action: Action(.open, "/Applications/Spotify.app")),
            Shortcut(title: "Play / pause", symbol: "playpause.fill", color: "#1DB954", action: Action(.media, "playpause")),
        ]
    }
}

/// The kinds of `Action` the daemon knows ("type" in JSON) and the field each one carries.
enum ActionKind: String, CaseIterable, Identifiable {
    case keys, open, shortcut, shell, micMute, media, clip, page

    var id: String { rawValue }

    var label: String {
        switch self {
        case .keys: return "Key combo"
        case .open: return "Open app, file or URL"
        case .shortcut: return "Run a Shortcut"
        case .shell: return "Shell command"
        case .micMute: return "Mute / unmute mic"
        case .media: return "Media control"
        case .clip: return "Play a clip"
        case .page: return "Go to page"
        }
    }

    /// The JSON field holding this kind's value (nil for micMute).
    var field: String? {
        switch self {
        case .keys: return "keys"
        case .open: return "target"
        case .shortcut: return "name"
        case .shell, .media: return "command"
        case .clip: return "title"
        case .page: return "page"
        case .micMute: return nil
        }
    }

    var defaultValue: String {
        switch self {
        case .media: return "playpause"
        case .page: return PageId.ai.rawValue
        default: return ""
        }
    }
}

/// A shortcut tile's action: {"type": "...", <field>: "..."}. Every field is a string, so the
/// action is kept as its type plus string fields; unknown types round-trip unchanged.
struct Action: Codable, Equatable {
    var type: String
    var fields: [String: String]

    init(_ kind: ActionKind, _ value: String? = nil) {
        type = kind.rawValue
        fields = kind.field.map { [$0: value ?? kind.defaultValue] } ?? [:]
    }

    var kind: ActionKind? { ActionKind(rawValue: type) }

    /// The value of this kind's one field.
    var value: String {
        get { kind?.field.flatMap { fields[$0] } ?? "" }
        set { if let f = kind?.field { fields[f] = newValue } }
    }

    var summary: String {
        guard let kind else { return type }
        switch kind {
        case .keys: return value.isEmpty ? "Keys" : value
        case .open: return "Open \(value)"
        case .shortcut: return "Shortcut “\(value)”"
        case .shell: return "$ \(value)"
        case .micMute: return "Mute mic"
        case .media: return "Media: \(value)"
        case .clip: return "Clip “\(value)”"
        case .page: return "Page: \(PageId(rawValue: value)?.label ?? value)"
        }
    }

    private struct Key: CodingKey {
        var stringValue: String
        var intValue: Int? { nil }
        init(_ s: String) { stringValue = s }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { nil }
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Key.self)
        type = (try? c.decode(String.self, forKey: Key("type"))) ?? ActionKind.keys.rawValue
        fields = [:]
        for k in c.allKeys where k.stringValue != "type" {
            if let s = try? c.decode(String.self, forKey: k) { fields[k.stringValue] = s }
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        try c.encode(type, forKey: Key("type"))
        if let kind {
            if let f = kind.field { try c.encode(fields[f] ?? kind.defaultValue, forKey: Key(f)) }
        } else {
            for (k, v) in fields { try c.encode(v, forKey: Key(k)) }
        }
    }
}

struct Soundboard: Codable, Equatable {
    /// Where clips live. Every audio file in it is a clip unless `clips` lists them.
    var folder = "~/Music/Quaked Soundboard"
    /// Your real mic (device name); "" = the system default input.
    var input = ""
    /// Where the mix goes: point your call app's microphone at this device.
    var output = "BlackHole 2ch"
    /// When the mic is routed through: "auto" (while an app listens to `output`), "on" or "off".
    var live = "auto"
    /// Mix your mic into the output (off = only clips go out).
    var passthrough = true
    /// Also play clips on your own speakers / headphones.
    var monitor = true
    /// "" = the system default output.
    var monitorDevice = ""
    /// Clip volume, 0.0 to 1.0.
    var gain = 0.8
    /// Optional order and styling; files not listed are added after these.
    var clips: [Clip] = []

    init() {}

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let d = Soundboard()
        folder = c.value(.folder, d.folder)
        input = c.value(.input, d.input)
        output = c.value(.output, d.output)
        live = c.value(.live, d.live)
        passthrough = c.value(.passthrough, d.passthrough)
        monitor = c.value(.monitor, d.monitor)
        monitorDevice = c.value(.monitorDevice, d.monitorDevice)
        gain = c.value(.gain, d.gain)
        clips = c.value(.clips, d.clips)
    }
}

struct Clip: Codable, Equatable {
    /// File name inside `folder`, or an absolute path.
    var file = ""
    var title = ""
    var color = ""
    var symbol = ""
    var gain = 1.0

    init(file: String) { self.file = file }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        file = c.value(.file, "")
        title = c.value(.title, "")
        color = c.value(.color, "")
        symbol = c.value(.symbol, "")
        gain = c.value(.gain, 1.0)
    }

    /// Whether this entry says anything beyond the defaults (only those are written).
    var isCustomized: Bool {
        !title.isEmpty || !color.isEmpty || !symbol.isEmpty || gain != 1.0
    }
}

extension Config {
    /// The config as pretty-printed JSON laid over `raw` (the file as last read), so keys this
    /// app doesn't know survive. Returns the bytes to write and the merged object.
    func fileData(keeping raw: [String: Any]) throws -> (Data, [String: Any]) {
        let mine = try JSONSerialization.jsonObject(with: JSONEncoder().encode(self)) as? [String: Any] ?? [:]
        let merged = Self.merge(raw, mine)
        let data = try JSONSerialization.data(withJSONObject: merged,
                                              options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes])
        return (data + Data("\n".utf8), merged)
    }

    /// `new` over `base`; nested objects merge, everything else (arrays included) is replaced.
    private static func merge(_ base: [String: Any], _ new: [String: Any]) -> [String: Any] {
        var out = base
        for (key, value) in new {
            if let b = base[key] as? [String: Any], let n = value as? [String: Any] {
                out[key] = merge(b, n)
            } else {
                out[key] = value
            }
        }
        return out
    }
}

extension KeyedDecodingContainer {
    /// The value for `key`, or `fallback` when it is missing or the wrong type.
    func value<T: Decodable>(_ key: Key, _ fallback: @autoclosure () -> T) -> T {
        ((try? decodeIfPresent(T.self, forKey: key)) ?? nil) ?? fallback()
    }
}

/// Clips are the audio files in the soundboard folder, ordered and styled by `clips` entries.
enum ClipLibrary {
    static let extensions: Set<String> = ["wav", "mp3", "m4a", "aac", "aiff", "aif", "flac", "caf"]

    struct Item: Identifiable {
        /// The file name in the folder, or the entry's own `file` when it points elsewhere.
        let file: String
        /// The `clips` entry styling it, if any.
        let entry: Clip?
        let exists: Bool

        var id: String { file }
        /// The key a `clips` entry for this item uses.
        var entryFile: String { entry?.file ?? file }
        var defaultTitle: String { ((file as NSString).lastPathComponent as NSString).deletingPathExtension }
        var title: String { (entry?.title).flatMap { $0.isEmpty ? nil : $0 } ?? defaultTitle }
    }

    static func expand(_ path: String) -> String { (path as NSString).expandingTildeInPath }

    /// Audio file names in `folder`, sorted like Finder.
    static func files(in folder: String) -> [String] {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: expand(folder))) ?? []
        return names
            .filter { extensions.contains(($0 as NSString).pathExtension.lowercased()) }
            .sorted { $0.localizedStandardCompare($1) == .orderedAscending }
    }

    static func items(_ sb: Soundboard) -> [Item] {
        let folder = expand(sb.folder)
        let names = files(in: sb.folder)
        var seen = Set<String>()
        var out: [Item] = []
        for entry in sb.clips {
            let path = entry.file.hasPrefix("/") || entry.file.hasPrefix("~")
                ? expand(entry.file) : (folder as NSString).appendingPathComponent(entry.file)
            let name = names.first { (folder as NSString).appendingPathComponent($0) == path }
            let item = Item(file: name ?? entry.file, entry: entry,
                            exists: name != nil || FileManager.default.fileExists(atPath: path))
            if seen.insert(item.id).inserted { out.append(item) }
        }
        for name in names where seen.insert(name).inserted {
            out.append(Item(file: name, entry: nil, exists: true))
        }
        return out
    }
}

/// Key combos as the daemon's src/keys.rs parses them: modifiers joined with "+" and one key.
enum KeyCombo {
    static let modifiers: Set<String> = ["cmd", "command", "super", "ctrl", "control", "alt", "opt", "option", "shift"]
    static let named: Set<String> = [
        "return", "enter", "escape", "esc", "delete", "backspace", "tab", "space", "minus", "equal",
        "forwarddelete", "del", "home", "pageup", "end", "pagedown", "right", "left", "down", "up",
        "mute", "volumeup", "volumedown",
    ]
    static let singles = Set("abcdefghijklmnopqrstuvwxyz0123456789-=[]\\;'`,./")

    static func isKey(_ k: String) -> Bool {
        if k.count == 1, let c = k.first { return singles.contains(c) }
        if k.hasPrefix("f"), let n = Int(k.dropFirst()) { return (1...24).contains(n) }
        return named.contains(k)
    }

    /// nil when `combo` is valid, otherwise what is wrong with it.
    static func problem(_ combo: String) -> String? {
        let parts = combo.split(separator: "+")
            .map { $0.trimmingCharacters(in: .whitespaces).lowercased() }
            .filter { !$0.isEmpty }
        if parts.isEmpty { return "Type a combo such as cmd+shift+4." }
        var key: String?
        for p in parts where !modifiers.contains(p) {
            guard isKey(p) else { return "Unknown key “\(p)”." }
            key = p
        }
        return key == nil ? "Add a key after the modifiers." : nil
    }
}
