#if canImport(UIKit)
import UIKit

/// UIView that intercepts Apple Pencil / stylus `UITouch`es and
/// republishes them as `AsyncThrowingStream<PenEvent, Error>` and
/// `AsyncThrowingStream<PenHoverEvent, Error>` for consumption by the
/// `istmo.pen` plugin backend.
///
/// Non-pencil touches are passed to `super` so ordinary finger input
/// keeps flowing through the responder chain — the view is a passive
/// observer, not a gesture consumer.
public final class PenCaptureView: UIView {

    public let events: AsyncThrowingStream<PenEvent, Error>
    public let hover: AsyncThrowingStream<PenHoverEvent, Error>

    private let eventsContinuation: AsyncThrowingStream<PenEvent, Error>.Continuation
    private let hoverContinuation: AsyncThrowingStream<PenHoverEvent, Error>.Continuation

    private var sequence: UInt32 = 0
    private let attachTime: TimeInterval = ProcessInfo.processInfo.systemUptime
    private var predictionEnabled: Bool = true

    public override init(frame: CGRect) {
        var eventsCont: AsyncThrowingStream<PenEvent, Error>.Continuation!
        self.events = AsyncThrowingStream<PenEvent, Error> { cont in
            eventsCont = cont
        }
        self.eventsContinuation = eventsCont

        var hoverCont: AsyncThrowingStream<PenHoverEvent, Error>.Continuation!
        self.hover = AsyncThrowingStream<PenHoverEvent, Error> { cont in
            hoverCont = cont
        }
        self.hoverContinuation = hoverCont

        super.init(frame: frame)

        let hoverGesture = UIHoverGestureRecognizer(
            target: self,
            action: #selector(handleHover(_:)),
        )
        addGestureRecognizer(hoverGesture)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("PenCaptureView must be constructed programmatically")
    }

    public var capabilities: PenCapabilities {
        PenCapabilities(
            pressure: true,
            tilt: true,
            azimuth: true,
            altitude: true,
            twist: false,
            tangentialPressure: false,
            hover: true,
            predicted: predictionEnabled,
            coalesced: true,
            // Apple Pencil hardware exposes no per-touch buttons.
            // Squeeze / double-tap on Pencil 2 / Pencil Pro are
            // delegate-driven `UIPencilInteraction` gestures, not
            // continuous button state — surface those through a
            // separate stream if / when a plugin needs them.
            buttonCount: 0,
            eraser: true,
        )
    }

    public func setPredictionEnabled(_ enabled: Bool) {
        predictionEnabled = enabled
    }

    public override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        forwardStylus(touches, event: event) { touch, event in
            let sample = self.sample(touch: touch)
            self.eventsContinuation.yield(.down(sample))
        }
        super.touchesBegan(touches, with: event)
    }

    public override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        forwardStylus(touches, event: event) { touch, event in
            let sample = self.sample(touch: touch)
            let coalesced = event?.coalescedTouches(for: touch)?
                .dropLast()
                .map { self.sample(touch: $0) } ?? []
            let predicted = self.predictionEnabled
                ? (event?.predictedTouches(for: touch)?.map { self.sample(touch: $0) } ?? [])
                : []
            self.eventsContinuation.yield(
                .move(PenMove(sample: sample, coalesced: Array(coalesced), predicted: predicted)),
            )
        }
        super.touchesMoved(touches, with: event)
    }

    public override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        forwardStylus(touches, event: event) { touch, _ in
            self.eventsContinuation.yield(.up(self.sample(touch: touch)))
        }
        super.touchesEnded(touches, with: event)
    }

    public override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        forwardStylus(touches, event: event) { touch, _ in
            self.eventsContinuation.yield(.cancel(self.sample(touch: touch)))
        }
        super.touchesCancelled(touches, with: event)
    }

    @objc private func handleHover(_ recognizer: UIHoverGestureRecognizer) {
        let point = recognizer.location(in: self)
        let zOffset: CGFloat
        if #available(iOS 16.4, *) {
            zOffset = recognizer.zOffset
        } else {
            zOffset = 0
        }
        let sample = self.hoverSample(location: point, zOffset: zOffset)
        switch recognizer.state {
        case .began:
            hoverContinuation.yield(.proximityEnter(sample))
        case .changed:
            hoverContinuation.yield(.move(sample))
        case .ended, .cancelled, .failed:
            hoverContinuation.yield(.proximityLeave)
        default:
            break
        }
    }

    private func forwardStylus(
        _ touches: Set<UITouch>,
        event: UIEvent?,
        handle: (UITouch, UIEvent?) -> Void,
    ) {
        for touch in touches where touch.type == .pencil {
            handle(touch, event)
        }
    }

    private func sample(touch: UITouch) -> PenSample {
        let point = touch.preciseLocation(in: self)
        let azimuth = touch.azimuthAngle(in: self)
        let altitude = touch.altitudeAngle
        let normalizedPressure = touch.maximumPossibleForce > 0
            ? Float(touch.force / touch.maximumPossibleForce)
            : 0
        let tilt = max(0, .pi / 2 - Float(altitude))
        let tiltX = tilt * sinf(Float(azimuth))
        let tiltY = -tilt * cosf(Float(azimuth))
        let time = touch.timestamp - attachTime
        return PenSample(
            x: Float(point.x),
            y: Float(point.y),
            pressure: normalizedPressure,
            tiltX: tiltX,
            tiltY: tiltY,
            azimuth: Float(azimuth),
            altitude: Float(altitude),
            twist: 0,
            tangentialPressure: 0,
            zOffset: 0,
            timestampUs: UInt64(max(0, time * 1_000_000)),
            sequence: nextSequence(),
            toolId: UInt32(bitPattern: Int32(truncatingIfNeeded: ObjectIdentifier(touch).hashValue)),
            toolKind: toolKind(touch),
            buttons: 0,
        )
    }

    private func hoverSample(location: CGPoint, zOffset: CGFloat) -> PenSample {
        PenSample(
            x: Float(location.x),
            y: Float(location.y),
            pressure: 0,
            tiltX: 0,
            tiltY: 0,
            azimuth: 0,
            altitude: 0,
            twist: 0,
            tangentialPressure: 0,
            zOffset: Float(zOffset),
            timestampUs: UInt64(max(0, (ProcessInfo.processInfo.systemUptime - attachTime) * 1_000_000)),
            sequence: nextSequence(),
            toolId: 0,
            toolKind: .tip,
            buttons: 0,
        )
    }

    private func toolKind(_ touch: UITouch) -> PenToolKind {
        // Apple Pencil does not expose an eraser tool type on `UITouch`
        // (ApplePencilPro's eraser mode is surfaced via `UIPencilInteraction`
        // preferences, not per-touch). Reported as `.tip` regardless.
        _ = touch
        return .tip
    }

    private func nextSequence() -> UInt32 {
        let s = sequence
        sequence &+= 1
        return s
    }
}

#endif
