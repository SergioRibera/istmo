import IstmoRuntime

@_silgen_name("istmo_run_ios")
func istmo_run_ios() -> Int32

// The backends are app-owned (istmo.toml sets `auto_register = false`), so
// they are registered here instead of through `IstmoPluginRegistry`.
IstmoApp.run(
    registerPlugins: {
        let runtime = IstmoRuntime.shared
        runtime.registerHandler(
            PermissionsDispatcher.PLUGIN_ID,
            PermissionsDispatcher(backend: PermissionsBackendImpl(), codecs: PermissionsCodecsImpl())
        )
        runtime.registerHandler(
            NotificationsDispatcher.PLUGIN_ID,
            NotificationsDispatcher(backend: NotificationsBackendImpl(), codecs: NotificationsCodecsImpl())
        )
        runtime.registerHandler(
            SignInDispatcher.PLUGIN_ID,
            SignInDispatcher(factory: SignInFactoryImpl(), codecs: SignInCodecsImpl())
        )
        runtime.registerHandler(
            AdMobDispatcher.PLUGIN_ID,
            AdMobDispatcher(factory: AdMobFactoryImpl(), codecs: AdMobCodecsImpl())
        )
    },
    entry: istmo_run_ios
)
