#if canImport(CoreVideo)
import CoreVideo

public extension IstmoFrameSink {
    /// Hand a pixel buffer to Rust, which retains it until the frame
    /// drops: the caller may let go of its own reference right away.
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

public extension IstmoFrameSink {
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
