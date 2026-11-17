// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "QuakedCompanion",
    platforms: [.macOS(.v13)],
    targets: [
        .executableTarget(name: "QuakedCompanion", path: "Sources/QuakedCompanion"),
    ]
)
