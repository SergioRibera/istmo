import Foundation

/// Live-update coalescing on Apple: a port of `src/coalesce.rs` (the
/// reference; its unit tests are the cases this mirrors).
///
/// Per id the latest pending update wins; a per-app token bucket
/// (`Rate.apple`: 2 per second, burst 2) limits posts. Alerting and final
/// updates post at once and spend a token (debt allowed, floored at
/// `-burst` tokens), a full show clears the id's dismissal and queue entry,
/// a dismissed id drops regular updates until an alert. Not thread-safe:
/// the backend calls it on its serial queue.
final class NotificationsCoalescer<Item> {
    /// Tokens per second and the bucket's size.
    struct Rate {
        let perSecond: Int64
        let burst: Int64

        /// `Rate::APPLE`.
        static var apple: Rate { Rate(perSecond: 2, burst: 2) }
    }

    enum Urgency {
        /// Silent update: coalesced, rate limited, dropped once dismissed.
        case regular
        /// Alerts again: posted at once, shows a dismissed one again.
        case alert
        /// The final content: posted at once, always.
        case finish
    }

    enum Admit {
        case now(Item)
        case queued
        case dropped
    }

    /// Milli-tokens per token.
    private static var token: Int64 { 1000 }

    let rate: Rate
    private var tokens: Int64
    private var refilledMs: Int64
    private var order: [String] = []
    private var pending: [String: Item] = [:]
    private var dismissedIds: Set<String> = []

    init(rate: Rate, nowMs: Int64) {
        self.rate = Rate(perSecond: max(rate.perSecond, 1), burst: max(rate.burst, 1))
        tokens = self.rate.burst * Self.token
        refilledMs = nowMs
    }

    private func refill(_ nowMs: Int64) {
        let elapsed = max(nowMs - refilledMs, 0)
        refilledMs = max(refilledMs, nowMs)
        let full = rate.burst * Self.token
        let (gained, overflow) = elapsed.multipliedReportingOverflow(by: rate.perSecond)
        tokens = overflow ? full : min(tokens &+ gained, full)
    }

    private func take() -> Bool {
        if tokens >= Self.token {
            tokens -= Self.token
            return true
        }
        return false
    }

    /// Spends a token even without one; the debt is floored at `-burst`.
    private func spend() {
        tokens = max(tokens - Self.token, -rate.burst * Self.token)
    }

    @discardableResult
    private func unqueue(_ id: String) -> Item? {
        guard let item = pending.removeValue(forKey: id) else { return nil }
        if let at = order.firstIndex(of: id) { order.remove(at: at) }
        return item
    }

    func offer(_ id: String, _ item: Item, _ urgency: Urgency, nowMs: Int64) -> Admit {
        refill(nowMs)
        switch urgency {
        case .regular:
            if dismissedIds.contains(id) { return .dropped }
            if pending[id] != nil {
                pending[id] = item
                return .queued
            }
            if order.isEmpty && take() { return .now(item) }
            order.append(id)
            pending[id] = item
            return .queued
        case .alert, .finish:
            dismissedIds.remove(id)
            unqueue(id)
            spend()
            return .now(item)
        }
    }

    /// A full show of `id`: forgets its dismissal and queued update.
    func shown(_ id: String, nowMs: Int64) {
        refill(nowMs)
        dismissedIds.remove(id)
        unqueue(id)
        spend()
    }

    func dismissed(_ id: String) {
        unqueue(id)
        dismissedIds.insert(id)
    }

    func cancel(_ id: String) {
        unqueue(id)
        dismissedIds.remove(id)
    }

    func clear() {
        order.removeAll()
        pending.removeAll()
        dismissedIds.removeAll()
    }

    /// The queued updates that may go now, oldest first.
    func poll(nowMs: Int64) -> [(String, Item)] {
        refill(nowMs)
        var out: [(String, Item)] = []
        while !order.isEmpty && take() {
            let id = order.removeFirst()
            if let item = pending.removeValue(forKey: id) { out.append((id, item)) }
        }
        return out
    }

    /// When the next queued update may go; nil when nothing is queued.
    func nextDue() -> Int64? {
        guard !order.isEmpty else { return nil }
        let missing = max(Self.token - tokens, 0)
        let wait = (missing + rate.perSecond - 1) / rate.perSecond
        return refilledMs + wait
    }

    var queued: Int { order.count }

    func isDismissed(_ id: String) -> Bool { dismissedIds.contains(id) }
}
