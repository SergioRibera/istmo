import Foundation
import IstmoRuntime

@_silgen_name("istmo_run_ios")
func istmo_run_ios() -> Int32

do {
    try IstmoRuntime.shared.start()
} catch {
    fatalError("IstmoRuntime.start() failed: \(error)")
}

IstmoRuntime.shared.registerHandler(
    BiometricDispatcher.PLUGIN_ID,
    BiometricDispatcher(backend: BiometricBackendImpl(), codecs: BiometricCodecsImpl())
)

_ = istmo_run_ios()
