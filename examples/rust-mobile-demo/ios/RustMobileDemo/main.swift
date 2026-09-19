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
    PermissionsDispatcher.PLUGIN_ID,
    PermissionsDispatcher(backend: PermissionsBackendImpl(), codecs: PermissionsCodecsImpl())
)
IstmoRuntime.shared.registerHandler(
    NotificationsDispatcher.PLUGIN_ID,
    NotificationsDispatcher(backend: NotificationsBackendImpl(), codecs: NotificationsCodecsImpl())
)
IstmoRuntime.shared.registerHandler(
    SignInDispatcher.PLUGIN_ID,
    SignInDispatcher(factory: SignInFactoryImpl(), codecs: SignInCodecsImpl())
)
IstmoRuntime.shared.registerHandler(
    AdMobDispatcher.PLUGIN_ID,
    AdMobDispatcher(factory: AdMobFactoryImpl(), codecs: AdMobCodecsImpl())
)

_ = istmo_run_ios()

