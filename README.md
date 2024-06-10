# ADB Client Implementation in Rust

Checkout [Android Source for Details about the used protocols](https://cs.android.com/android/platform/superproject/main/+/main:packages/modules/adb/)


## Development

To inspect adb traffic you can e.g. use SOCAT like this:
```socat -x -v TCP-LISTEN:8080,fork TCP:127.0.0.1:5037```