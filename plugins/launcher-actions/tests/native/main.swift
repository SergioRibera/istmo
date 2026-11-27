// Prints `LauncherActionLink.encode` for the cases of
// `tests/deep_link_wire.rs`, one `label hex` line each.
import Foundation

let digits = Array("0123456789abcdef")

func hex(_ data: Data) -> String {
    String(data.flatMap { [digits[Int($0 >> 4)], digits[Int($0 & 0x0f)]] })
}

func emit(_ label: String, _ uri: String, _ id: String?, _ ms: UInt64?) {
    let source = id.map { LauncherActionLink.sourcePrefix + $0 }
    print("\(label) \(hex(LauncherActionLink.encode(uri: uri, source: source, receivedAtMs: ms)))")
}

emit("full", "notes://compose", "compose", 1_700_000_000_123)
emit("bare", "notes://x", nil, nil)
emit("small-ms", "com.example.notes:/a?b=c", "a.b-c_9", 250)
emit("mid-ms", "notes://é", "x", 65_536)
