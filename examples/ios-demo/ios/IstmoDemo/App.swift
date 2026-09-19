import IstmoRuntime
import SwiftUI

@main
struct IstmoDemoApp: App {

    init() {

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

