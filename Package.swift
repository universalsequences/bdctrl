// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "beadsgpu",
    platforms: [.macOS(.v14)],
    products: [.executable(name: "beadsgpu", targets: ["BeadsGPU"])],
    targets: [
        .executableTarget(
            name: "BeadsGPU",
            path: "Sources/BeadsGPU"
        ),
        .testTarget(
            name: "BeadsGPUTests",
            dependencies: ["BeadsGPU"],
            path: "Tests/BeadsGPUTests"
        )
    ]
)
