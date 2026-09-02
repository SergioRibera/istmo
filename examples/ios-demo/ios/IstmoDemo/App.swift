import IstmoRuntime
import SwiftUI

@main
struct IstmoDemoApp: App {

    init() {
        // Bring the transport up before any view can call into it.
        // Failure here is fatal — every path the UI takes goes through
        // IstmoRuntime.shared.
        do {
            try IstmoRuntime.shared.start()
        } catch {
            fatalError("IstmoRuntime.start() failed: \(error)")
        }
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }
}
