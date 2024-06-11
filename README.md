[![Build](https://github.com/devicelink/adb-client/actions/workflows/build.yaml/badge.svg)](https://github.com/devicelink/adb-client/actions/workflows/build.yaml)
[![Crates.io](https://img.shields.io/crates/v/adb-client.svg)](https://crates.io/crates/adb-client-tokio)
[![Docs.rs](https://docs.rs/adb-client-tokio/badge.svg)](https://docs.rs/adb-client-tokio)

# Android Debug Bridge (ADB) Client Library for async Rust

A pure rust implementation to send commands and forwards traffic to an android device using a adb server.

# Examples

Run a shell command on an device:

```rust
        let adb_client = AdbClient::connect_tcp("127.0.0.1:5037").await?;
        let manufaturer: String = adb_client
            .shell("MY_SERIAL", "getprop ro.product.manufacturer")
            .await?;
```

## Protocol Details


Checkout [Android Source](https://cs.android.com/android/platform/superproject/main/+/main:packages/modules/adb/) for Details about the used protocols

## Development

To inspect adb traffic you can e.g. use SOCAT like this:  
```socat -x -v TCP-LISTEN:8080,fork TCP:127.0.0.1:5037```