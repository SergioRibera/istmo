// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "istmo",
    platforms: [
        .iOS(.v14),
        .tvOS(.v14),
        .watchOS(.v7),
        .visionOS(.v1),
    ],
    products: [
        .library(name: "IstmoRuntime", targets: ["IstmoRuntime"]),
    ],
    targets: [
        // C declarations of the `istmo-ios` exports; the definitions come
        // from the app's Rust staticlib at link time.
        .target(
            name: "IstmoFFI",
            path: "runtime/ios/Sources/IstmoFFI"
        ),
        .target(
            name: "IstmoRuntime",
            dependencies: ["IstmoFFI"],
            path: "runtime/ios/Sources/IstmoRuntime"
        ),
    ]
)
