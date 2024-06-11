[![Crates.io](https://img.shields.io/crates/v/adb-client.svg)](https://crates.io/crates/adb-client-tokio)
[![Docs.rs](https://docs.rs/adb-client-tokio/badge.svg)](https://docs.rs/adb-client-tokio)
[![Build](https://github.com/devicelink/adb-client-tokio/actions/workflows/test.yml/badge.svg?branch=main)](https://github.com/devicelink/adb-client-tokio/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

# Android Debug Bridge (ADB) Client Library for async Rust

A pure rust implementation to send commands and forwards traffic to an android device using a adb server.

# Complete Example

Run a shell command on an device:

```rust
use adb_client_tokio::{Device, AdbClient};
use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut adb = AdbClient::connect_tcp("127.0.0.1:5037").await.unwrap();
    let version = adb.shell(Device::Default, "getprop ro.product.model").await?;
    println!("ADB server version: {}", version);
Ok(())
}
```

## Protocol Details

Checkout [Android Source](https://cs.android.com/android/platform/superproject/main/+/main:packages/modules/adb/) for details about the used protocols

## Development

To inspect adb traffic you can e.g. use SOCAT like this:  
```socat -x -v TCP-LISTEN:8080,fork TCP:127.0.0.1:5037```