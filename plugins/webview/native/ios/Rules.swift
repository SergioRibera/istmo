/// Swift twin of the Rust `rules` module, held to
/// `tests/rule_vectors.tsv`. Pure Swift (no Foundation URL types), so
/// the vectors run on any Swift toolchain.
struct CompiledRules {
    private static let blocked: Set<String> = ["javascript", "intent", "data", "vbscript"]
    static let defaultPorts: [String: Int] = ["http": 80, "https": 443, "ws": 80, "wss": 443, "ftp": 21]

    private struct Pattern {
        let scheme: String?
        let host: String?
        let subdomains: Bool
        let port: Int?
        let pathPrefix: String

        func matches(_ url: RuleUrl) -> Bool {
            if let scheme = scheme, scheme != url.scheme { return false }
            if let host = host {
                guard let got = url.host else { return false }
                if subdomains {
                    guard got.hasSuffix("." + host), got.count > host.count + 1 else { return false }
                } else if got != host {
                    return false
                }
            }
            let portOk = port.map { url.effectivePort == $0 } ?? (url.explicitPort == nil)
            return portOk && CompiledRules.pathMatches(pathPrefix, url.path)
        }
    }

    private let defaultAction: RuleAction
    private let patterns: [(Pattern, RuleAction)]

    init(_ rules: NavigationRules) throws {
        defaultAction = rules.defaultAction
        patterns = try rules.rules.map { (try CompiledRules.compile($0.pattern), $0.action) }
    }

    func evaluate(_ raw: String, fileAccess: Bool) -> RuleAction {
        guard let url = RuleUrl.parse(raw) else { return .deny }
        if CompiledRules.blocked.contains(url.scheme) || (url.scheme == "file" && !fileAccess) || url.hasCredentials {
            return .deny
        }
        return patterns.first { $0.0.matches(url) }?.1 ?? defaultAction
    }

    private static func compile(_ p: UrlPattern) throws -> Pattern {
        let bad = WebViewError.invalidArgument("url pattern \(p.scheme)://\(p.host)\(p.pathPrefix)")
        var scheme: String?
        if p.scheme != "*" {
            let lower = p.scheme.lowercased()
            guard isScheme(lower) else { throw bad }
            scheme = lower
        }
        var host: String?
        var subdomains = false
        if p.host != "*" {
            var domain = Substring(p.host)
            if domain.hasPrefix("*.") {
                subdomains = true
                domain = domain.dropFirst(2)
            }
            guard !domain.isEmpty, !domain.contains(where: { "*/@?#".contains($0) }),
                  let normalized = normalizeHost(String(domain))
            else { throw bad }
            host = normalized
        }
        var path = ""
        if !p.pathPrefix.isEmpty {
            guard p.pathPrefix.hasPrefix("/") else { throw bad }
            path = RuleUrl.parse("https://h" + p.pathPrefix)?.path ?? "/"
            while path.count > 1 && path.hasSuffix("/") { path.removeLast() }
        }
        return Pattern(scheme: scheme, host: host, subdomains: subdomains, port: p.port.map(Int.init), pathPrefix: path)
    }

    static func isScheme(_ s: String) -> Bool {
        guard let first = s.first, first.isASCII, first.isLowercase else { return false }
        return s.allSatisfy { $0.isASCII && ($0.isLowercase || $0.isNumber || "+-.".contains($0)) }
    }

    static func pathMatches(_ prefix: String, _ path: String) -> Bool {
        if prefix.isEmpty || prefix == "/" { return true }
        guard path.hasPrefix(prefix) else { return false }
        let rest = path.dropFirst(prefix.count)
        return rest.isEmpty || rest.hasPrefix("/")
    }

    /// Lowercase, punycode, no trailing dot, WHATWG IPv4 forms.
    static func normalizeHost(_ raw: String) -> String? {
        if raw.hasPrefix("[") { return raw.lowercased() }
        var trimmed = Substring(raw)
        while trimmed.hasSuffix(".") { trimmed = trimmed.dropLast() }
        guard !trimmed.isEmpty else { return nil }
        var labels: [String] = []
        for label in trimmed.lowercased().split(separator: ".", omittingEmptySubsequences: false) {
            if label.allSatisfy({ $0.isASCII }) {
                labels.append(String(label))
            } else {
                guard let encoded = Punycode.encode(String(label)) else { return nil }
                labels.append("xn--" + encoded)
            }
        }
        let ascii = labels.joined(separator: ".")
        return parseIpv4(ascii) ?? ascii
    }

    private static func parseIpv4(_ host: String) -> String? {
        var parts = host.split(separator: ".", omittingEmptySubsequences: false).map(String.init)
        if parts.count > 1 && parts.last == "" { parts.removeLast() }
        guard !parts.isEmpty, parts.count <= 4 else { return nil }
        var numbers: [UInt64] = []
        for part in parts {
            guard let n = parseIpv4Number(part) else { return nil }
            numbers.append(n)
        }
        if numbers.dropLast().contains(where: { $0 > 255 }) { return nil }
        guard let last = numbers.last, last < (UInt64(1) << (8 * (5 - UInt64(numbers.count)))) else { return nil }
        var value = last
        for (i, n) in numbers.dropLast().enumerated() { value += n << (8 * (3 - UInt64(i))) }
        return (0..<4).reversed().map { String((value >> (8 * UInt64($0))) & 0xff) }.joined(separator: ".")
    }

    private static func parseIpv4Number(_ part: String) -> UInt64? {
        guard !part.isEmpty else { return nil }
        var digits = Substring(part)
        var radix = 10
        if digits.hasPrefix("0x") || digits.hasPrefix("0X") {
            digits = digits.dropFirst(2)
            radix = 16
        } else if digits.count > 1 && digits.hasPrefix("0") {
            digits = digits.dropFirst()
            radix = 8
        }
        if digits.isEmpty { return 0 }
        return UInt64(digits, radix: radix)
    }

    /// `scheme://host[:port]` of `raw`, `nil` for opaque origins.
    static func origin(of raw: String) -> String? {
        guard let url = RuleUrl.parse(raw), let host = url.host, defaultPorts[url.scheme] != nil else { return nil }
        return url.scheme + "://" + host + (url.explicitPort.map { ":\($0)" } ?? "")
    }
}

/// The pieces of a URL the rules look at, normalised.
struct RuleUrl {
    let scheme: String
    let host: String?
    let explicitPort: Int?
    let path: String
    let hasCredentials: Bool

    var effectivePort: Int? { explicitPort ?? CompiledRules.defaultPorts[scheme] }

    static func parse(_ input: String) -> RuleUrl? {
        let raw = input.trimmingSpaces()
        guard let colon = raw.firstIndex(of: ":") else { return nil }
        let scheme = raw[..<colon].lowercased()
        guard CompiledRules.isScheme(scheme) else { return nil }
        var rest = String(raw[raw.index(after: colon)...])
        let special = CompiledRules.defaultPorts[scheme] != nil || scheme == "file"
        if special { rest = rest.replacingAll("\\", with: "/") }
        if rest.contains(" ") { return nil }
        // Drop query and fragment.
        if let cut = rest.firstIndex(where: { $0 == "?" || $0 == "#" }) { rest = String(rest[..<cut]) }
        var host: String?
        var port: Int?
        var credentials = false
        var path = rest
        if rest.hasPrefix("//") {
            let afterSlashes = rest.dropFirst(2)
            let end = afterSlashes.firstIndex(of: "/") ?? afterSlashes.endIndex
            var authority = String(afterSlashes[..<end])
            path = String(afterSlashes[end...])
            if let at = authority.lastIndex(of: "@") {
                credentials = true
                authority = String(authority[authority.index(after: at)...])
            }
            var hostPart = authority
            if authority.hasPrefix("[") {
                guard let close = authority.firstIndex(of: "]") else { return nil }
                hostPart = String(authority[...close])
                let tail = authority[authority.index(after: close)...]
                if tail.hasPrefix(":") { guard let p = Int(tail.dropFirst()) else { return nil }; port = p }
            } else if let c = authority.lastIndex(of: ":") {
                hostPart = String(authority[..<c])
                let portText = authority[authority.index(after: c)...]
                if !portText.isEmpty { guard let p = Int(portText), p <= 65535 else { return nil }; port = p }
            }
            if !hostPart.isEmpty {
                guard let normalized = CompiledRules.normalizeHost(hostPart) else { return nil }
                host = normalized
            } else if special && scheme != "file" {
                return nil
            }
        } else if special && scheme != "file" {
            return nil
        }
        if port == CompiledRules.defaultPorts[scheme] { port = nil }
        let hierarchical = host != nil || special || path.hasPrefix("/")
        return RuleUrl(scheme: scheme, host: host, explicitPort: port,
                       path: hierarchical ? normalizePath(path) : path, hasCredentials: credentials)
    }

    private static func normalizePath(_ raw: String) -> String {
        if raw.isEmpty { return "/" }
        var out: [Substring] = []
        let segments = raw.split(separator: "/", omittingEmptySubsequences: false).dropFirst()
        for segment in segments {
            switch segment.lowercased() {
            case ".", "%2e": continue
            case "..", ".%2e", "%2e.", "%2e%2e": if !out.isEmpty { out.removeLast() }
            default: out.append(segment)
            }
        }
        let trailing = raw.hasSuffix("/") || raw.hasSuffix("/.") || raw.hasSuffix("/..")
        return "/" + out.joined(separator: "/") + (trailing && !out.isEmpty ? "/" : "")
    }
}

/// RFC 3492 encoder for one label.
enum Punycode {
    static func encode(_ input: String) -> String? {
        let scalars = input.unicodeScalars.map { $0.value }
        var output = scalars.filter { $0 < 0x80 }.map { Character(Unicode.Scalar($0)!) }
        let basic = output.count
        var handled = basic
        if basic > 0 { output.append("-") }
        var n: UInt32 = 128, delta: UInt32 = 0, bias: UInt32 = 72
        while handled < scalars.count {
            guard let m = scalars.filter({ $0 >= n }).min() else { return nil }
            delta += (m - n) * UInt32(handled + 1)
            n = m
            for c in scalars {
                if c < n { delta += 1 }
                if c == n {
                    var q = delta
                    var k: UInt32 = 36
                    while true {
                        let t = k <= bias ? 1 : (k >= bias + 26 ? 26 : k - bias)
                        if q < t { break }
                        output.append(digit(t + (q - t) % (36 - t)))
                        q = (q - t) / (36 - t)
                        k += 36
                    }
                    output.append(digit(q))
                    bias = adapt(delta, UInt32(handled + 1), handled == basic)
                    delta = 0
                    handled += 1
                }
            }
            delta += 1
            n += 1
        }
        return String(output)
    }

    private static func digit(_ d: UInt32) -> Character {
        Character(Unicode.Scalar(d < 26 ? d + 97 : d + 22)!)
    }

    private static func adapt(_ delta: UInt32, _ points: UInt32, _ first: Bool) -> UInt32 {
        var d = first ? delta / 700 : delta / 2
        d += d / points
        var k: UInt32 = 0
        while d > 455 { d /= 35; k += 36 }
        return k + 36 * d / (d + 38)
    }
}

private extension String {
    func trimmingSpaces() -> String {
        var s = Substring(self)
        while s.first == " " { s = s.dropFirst() }
        while s.last == " " { s = s.dropLast() }
        return String(s)
    }

    func replacingAll(_ target: Character, with replacement: Character) -> String {
        String(map { $0 == target ? replacement : $0 })
    }
}
