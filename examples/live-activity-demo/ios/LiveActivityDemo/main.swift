import Foundation
import IstmoRuntime

@_silgen_name("istmo_run_ios")
func istmo_run_ios() -> Int32

do {
    try IstmoRuntime.shared.start()
} catch {
    fatalError("IstmoRuntime.start() failed: \(error)")
}

let backend = LiveActivityBackendImpl()
if #available(iOS 16.1, *) {
    backend.register(handler: TimerLiveActivityHandler())
}

IstmoRuntime.shared.registerHandler(
    LiveActivityDispatcher.PLUGIN_ID,
    LiveActivityDispatcher(backend: backend, codecs: LiveActivityCodecsImpl())
)

_ = istmo_run_ios()

