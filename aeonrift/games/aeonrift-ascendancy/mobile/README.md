# Ascendancy mobile packaging

The same `cards` cdylib and `#[bevy_main]` entry point ships on both platforms.
No credentials are embedded in the app; the product API supplies profile,
collection, offer, order, and entitlement state at runtime.

## Android

Install `cargo-apk`, the Android SDK/NDK, then run from the `aeonrift` root:

```sh
cargo apk build -p aeonrift-ascendancy --features client --release
```

The application id, SDK floor, targets, and label live in the crate's
`[package.metadata.android]` section.

## iOS

Install the Rust Apple targets and build the cdylib consumed by the Xcode host:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
cargo build -p aeonrift-ascendancy --features client --lib --release --target aarch64-apple-ios
cargo build -p aeonrift-ascendancy --features client --lib --release --target aarch64-apple-ios-sim
```

Use `ios/Info.plist` and `ios/PrivacyInfo.xcprivacy` in the host target. Signing,
team selection, and App Store provisioning remain intentionally outside source.
