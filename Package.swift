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
        .target(
            name: "IstmoRuntime",
            path: "runtime/ios/Sources/IstmoRuntime"
        ),
    ]
)

