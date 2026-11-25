#if canImport(CoreVideo)
import CoreVideo

public extension IstmoFrameSender {
    /// Hand a pixel buffer to Rust, which retains it until the frame
    /// drops: the caller may let go of its own reference right away, and
    /// the buffer's pool does not reuse it while Rust holds it.
    func submit(_ pixelBuffer: CVPixelBuffer, meta: M) -> IstmoSubmitStatus {
        submitRetained(
            Unmanaged.passRetained(pixelBuffer).toOpaque(),
            kind: .cvPixelBuffer,
            release: { object in
                guard let object else { return }
                Unmanaged<CVPixelBuffer>.fromOpaque(object).release()
            },
            meta: meta)
    }
}
#endif

#if canImport(IOSurface)
import IOSurface

public extension IstmoFrameSender {
    /// Hand an `IOSurface` to Rust, which retains it until the frame
    /// drops.
    func submit(_ surface: IOSurfaceRef, meta: M) -> IstmoSubmitStatus {
        submitRetained(
            Unmanaged.passRetained(surface).toOpaque(),
            kind: .ioSurface,
            release: { object in
                guard let object else { return }
                Unmanaged<IOSurfaceRef>.fromOpaque(object).release()
            },
            meta: meta)
    }
}
#endif

#if canImport(CoreMedia)
import CoreMedia

public extension IstmoFrameSender {
    /// Hand a sample buffer (a ReplayKit / AVCapture / ScreenCaptureKit
    /// callback's) to Rust, which retains it until the frame drops.
    func submit(_ sampleBuffer: CMSampleBuffer, meta: M) -> IstmoSubmitStatus {
        submitRetained(
            Unmanaged.passRetained(sampleBuffer).toOpaque(),
            kind: .cmSampleBuffer,
            release: { object in
                guard let object else { return }
                Unmanaged<CMSampleBuffer>.fromOpaque(object).release()
            },
            meta: meta)
    }
}
#endif
