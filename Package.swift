// swift-tools-version:5.9
//
// Root SPM manifest for the istmo monorepo.
//
// SPM does not support subpath references — a `Package.swift` must live
// at the root of the resolved git tree. Consumers therefore add the whole
// repo as a package dependency:
//
//     .package(url: "https://github.com/SergioRibera/istmo.git", from: "0.1.0")
//
// and reference the `IstmoRuntime` product. Rust sources, Kotlin sources
// and every other language artefact coexist alongside this manifest; SPM
// ignores anything outside the paths declared below.
//
// The Kotlin sibling lives under `runtime/android/` and is published as
// `dev.istmo:istmo-runtime` via the GitHub Packages Maven
// registry — see `runtime/README.md` for consumer setup.

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
