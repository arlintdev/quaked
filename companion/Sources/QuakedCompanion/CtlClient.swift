// Talks to the running daemon over its control socket (~/.claude/quaked/ctl.sock):
// one request per connection, a line out, then everything up to EOF back.

import AppKit
import Darwin

enum CtlError: Error {
    /// No socket, or nobody listening on it.
    case notRunning
    case io(String)
}

struct CtlClient {
    var path = NSHomeDirectory() + "/.claude/quaked/ctl.sock"

    /// Blocking request; call it off the main thread (see `send`).
    func request(_ line: String, timeout: TimeInterval = 2) -> Result<Data, CtlError> {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return .failure(.io(Self.lastError())) }
        defer { close(fd) }

        var on: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))
        var tv = timeval(tv_sec: Int(timeout), tv_usec: Int32((timeout - floor(timeout)) * 1_000_000))
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = Array(path.utf8)
        guard pathBytes.count < MemoryLayout.size(ofValue: addr.sun_path) else {
            return .failure(.io("socket path too long"))
        }
        withUnsafeMutableBytes(of: &addr.sun_path) { buf in
            buf.copyBytes(from: pathBytes)
            buf[pathBytes.count] = 0
        }
        addr.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        let connected = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard connected == 0 else {
            return errno == ENOENT || errno == ECONNREFUSED ? .failure(.notRunning) : .failure(.io(Self.lastError()))
        }

        let clean = line.replacingOccurrences(of: "\n", with: " ").replacingOccurrences(of: "\r", with: " ")
        let message = Array((clean + "\n").utf8)
        var sent = 0
        while sent < message.count {
            let n = message[sent...].withUnsafeBytes { write(fd, $0.baseAddress, $0.count) }
            if n < 0 && errno == EINTR { continue }
            guard n > 0 else { return .failure(.io(Self.lastError())) }
            sent += n
        }
        // Half-close: the request is complete, so a daemon reading lines until EOF answers now.
        shutdown(fd, SHUT_WR)

        var reply = Data()
        var buf = [UInt8](repeating: 0, count: 64 * 1024)
        while true {
            let n = read(fd, &buf, buf.count)
            if n > 0 {
                reply.append(buf, count: n)
            } else if n == 0 {
                return .success(reply)
            } else if errno != EINTR {
                return .failure(.io(errno == EAGAIN ? "timed out" : Self.lastError()))
            }
        }
    }

    func send(_ line: String, timeout: TimeInterval = 2) async -> Result<Data, CtlError> {
        await withCheckedContinuation { cont in
            DispatchQueue.global(qos: .userInitiated).async {
                cont.resume(returning: request(line, timeout: timeout))
            }
        }
    }

    private static func lastError() -> String { String(cString: strerror(errno)) }
}

// MARK: - Replies

struct DaemonStatus: Decodable, Equatable {
    struct Track: Decodable, Equatable {
        var title: String?
        var artist: String?
        var playing: Bool?
    }

    struct SoundboardState: Decodable, Equatable {
        var routing: Bool?
        var error: String?
        var playing: [String]?
        var micMuted: Bool?
        /// Bundle id of the app listening to the output device, e.g. "us.zoom.xos".
        var listener: String?
    }

    var version: String?
    var connected: Bool?
    var page: String?
    var level: String?
    var sessions: Int?
    var waiting: Int?
    var working: Int?
    var track: Track?
    var soundboard: SoundboardState?
}

struct AudioDevices: Decodable, Equatable {
    var inputs: [String] = []
    var outputs: [String] = []

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        inputs = c.value(.inputs, [])
        outputs = c.value(.outputs, [])
    }

    enum CodingKeys: String, CodingKey { case inputs, outputs }
}

// MARK: - Daemon state for the UI

enum Reachability {
    case unknown
    /// No socket or nobody listening.
    case notRunning
    /// Listening, but it does not answer `status` (an older build).
    case outdated
    case running
}

@MainActor
final class DaemonModel: ObservableObject {
    let ctl = CtlClient()
    @Published private(set) var reach: Reachability = .unknown
    @Published private(set) var status: DaemonStatus?
    @Published private(set) var devices: AudioDevices?
    /// The last command's error, shown in the banner for a few seconds.
    @Published var message: String?
    private var messageClear: Task<Void, Never>?

    init() {
        Task { [weak self] in
            while let self, !Task.isCancelled {
                await self.refreshStatus()
                let active = NSApp?.isActive ?? true
                let seconds: UInt64 = self.reach == .outdated ? 10 : (active ? 2 : 8)
                try? await Task.sleep(nanoseconds: seconds * 1_000_000_000)
            }
        }
    }

    func refreshStatus() async {
        let result = await ctl.send("status")
        switch result {
        case .failure:
            reach = .notRunning
            status = nil
        case .success(let data):
            let line = data.split(separator: UInt8(ascii: "\n")).first.map { Data($0) } ?? Data()
            if let s = try? JSONDecoder().decode(DaemonStatus.self, from: line) {
                if status != s { status = s }
                if reach != .running {
                    reach = .running
                    if devices == nil { await loadDevices() }
                }
            } else {
                reach = .outdated
                status = nil
            }
        }
    }

    func loadDevices() async {
        if case .success(let data) = await ctl.send("audio-devices"),
           let d = try? JSONDecoder().decode(AudioDevices.self, from: data) {
            devices = d
        }
    }

    /// The screen as PNG bytes, or nil when the daemon can't give one.
    func frame() async -> Data? {
        if case .success(let data) = await ctl.send("frame", timeout: 3), !data.isEmpty { return data }
        return nil
    }

    /// Sends a command; returns whether the daemon accepted it ("ok", or silence from older builds).
    @discardableResult
    func command(_ line: String, quietIfDown: Bool = false) async -> Bool {
        switch await ctl.send(line) {
        case .failure(.notRunning):
            reach = .notRunning
            if !quietIfDown { show("quaked isn't running, so “\(line)” went nowhere.") }
            return false
        case .failure(.io(let why)):
            show("“\(line)” failed: \(why)")
            return false
        case .success(let data):
            let reply = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
            if reply.isEmpty || reply == "ok" { return true }
            show("quaked: \(reply)")
            return false
        }
    }

    func fire(_ line: String) {
        Task { await command(line) }
    }

    func show(_ text: String) {
        message = text
        messageClear?.cancel()
        messageClear = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 8_000_000_000)
            if !Task.isCancelled { self?.message = nil }
        }
    }
}
