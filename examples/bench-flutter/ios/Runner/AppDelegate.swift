import UIKit
import Flutter

@main
@objc class AppDelegate: FlutterAppDelegate {
    override func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
    ) -> Bool {
        GeneratedPluginRegistrant.register(with: self)

        guard let controller = window?.rootViewController as? FlutterViewController
        else { fatalError("Expected FlutterViewController as root") }

        let channel = FlutterMethodChannel(
            name: "dev.istmo.bench/echo",
            binaryMessenger: controller.binaryMessenger
        )
        channel.setMethodCallHandler { call, result in
            if call.method == "echo", let bytes = call.arguments as? FlutterStandardTypedData {
                result(bytes)
            } else {
                result(FlutterMethodNotImplemented)
            }
        }

        // Enable after running the Pigeon generator for Swift / FFI:
        // BenchHostSetup.setUp(binaryMessenger: controller.binaryMessenger, api: BenchHostImpl())

        return super.application(application, didFinishLaunchingWithOptions: launchOptions)
    }
}

// Stub for the Pigeon-generated `BenchHost` protocol. Uncomment once
// `BenchPigeon.swift` is produced.
//
// final class BenchHostImpl: BenchHost {
//     func echo(payload: FlutterStandardTypedData) throws -> FlutterStandardTypedData {
//         return payload
//     }
// }
