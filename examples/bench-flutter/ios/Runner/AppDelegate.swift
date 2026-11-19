import UIKit
import Flutter
import os

@main
@objc class AppDelegate: FlutterAppDelegate {
    private let log = OSLog(subsystem: "dev.istmo.bench", category: "istmo-flutter-bench")

    override func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
    ) -> Bool {
        GeneratedPluginRegistrant.register(with: self)

        guard let controller = window?.rootViewController as? FlutterViewController
        else { fatalError("Expected FlutterViewController as root") }

        let echo = FlutterMethodChannel(
            name: "dev.istmo.bench/echo",
            binaryMessenger: controller.binaryMessenger
        )
        echo.setMethodCallHandler { [weak self] call, result in
            switch call.method {
            case "echo":
                if let bytes = call.arguments as? FlutterStandardTypedData { result(bytes) }
                else { result(FlutterMethodNotImplemented) }
            case "noop":
                result(nil)
            case "noopBytes":
                // Argument arrives decoded; ignore it.
                _ = call.arguments as? FlutterStandardTypedData
                result(nil)
            default:
                result(FlutterMethodNotImplemented)
            }
            _ = self // silence warning
        }

        let report = FlutterMethodChannel(
            name: "dev.istmo.bench/report",
            binaryMessenger: controller.binaryMessenger
        )
        report.setMethodCallHandler { [weak self] call, result in
            guard let self = self else { result(nil); return }
            switch call.method {
            case "begin":
                os_log("BENCH_BEGIN", log: self.log, type: .info)
                result(nil)
            case "line":
                let line = call.arguments as? String ?? ""
                os_log("%{public}@", log: self.log, type: .info, line)
                result(nil)
            case "end":
                os_log("BENCH_END", log: self.log, type: .info)
                result(nil)
            case "shouldAutoRun":
                // iOS launch arguments: `-auto YES` passed via
                // `xcrun simctl launch … -auto YES` or Xcode scheme.
                let auto = ProcessInfo.processInfo.arguments.contains("-auto")
                    && ProcessInfo.processInfo.arguments.contains("YES")
                result(auto)
            default:
                result(FlutterMethodNotImplemented)
            }
        }

        // Pigeon registration once the generator has run:
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
