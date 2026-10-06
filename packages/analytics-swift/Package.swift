// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "RailyardAnalytics",
    platforms: [.iOS(.v15), .macOS(.v12), .tvOS(.v15), .watchOS(.v8)],
    products: [
        .library(name: "RailyardAnalytics", targets: ["RailyardAnalytics"]),
    ],
    targets: [
        .target(name: "RailyardAnalytics"),
        .testTarget(name: "RailyardAnalyticsTests", dependencies: ["RailyardAnalytics"]),
    ]
)
