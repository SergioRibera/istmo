import IstmoRuntime

@_silgen_name("istmo_run_ios")
func istmo_run_ios() -> Int32

// istmo.toml turns off auto-registration for istmo.live_activity: the
// backend is built here to attach the demo's timer handler.
IstmoApp.run(
    registerPlugins: {
        let backend = LiveActivityBackendImpl()
        if #available(iOS 16.1, *) {
            backend.register(handler: TimerLiveActivityHandler())
        }
        IstmoRuntime.shared.registerHandler(
            LiveActivityDispatcher.PLUGIN_ID,
            LiveActivityDispatcher(backend: backend, codecs: LiveActivityCodecsImpl())
        )
    },
    entry: istmo_run_ios
)
