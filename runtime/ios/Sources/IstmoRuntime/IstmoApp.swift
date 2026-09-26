import Foundation

/// Entry point of a Rust-driven istmo app.
///
/// `istmo-build` generates an `IstmoApp.run()` overload into the app
/// (`Plugins/IstmoMain.swift`) for crates with a `#[istmo::mobile_app]`
/// entry point, so the app's `main.swift` is the single line
/// `IstmoApp.run()`.
public enum IstmoApp {
    /// Start the runtime, call `registerPlugins`, then hand the process
    /// over to `entry` — the crate's `istmo_run_ios`, which runs the UI
    /// event loop and normally never returns.
    public static func run(registerPlugins: () -> Void, entry: () -> Int32) -> Never {
        do {
            try IstmoRuntime.shared.start()
        } catch {
            fatalError("istmo: IstmoRuntime.start() failed: \(error)")
        }
        registerPlugins()
        exit(entry())
    }
}
