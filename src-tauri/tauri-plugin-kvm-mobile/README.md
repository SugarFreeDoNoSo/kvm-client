# Native Android KVM session

Local Tauri plugin for Android. Kotlin owns WebRTC video decoding/rendering, the
`rpc` DataChannel, relative touch motion, double-tap detection, paced USB HID
keyboard reports, and the Android IME. React only controls UI, viewport geometry
and low-frequency signaling. Rust retains HTTPS authentication, WSS and explicit
certificate fingerprint trust using the shared `net.rs` implementation.

The peer receives audio/video and never requests camera/microphone capture.
Touch coordinates use density-independent units. Motion is accumulated every
8 ms and split into HID-sized reports; keyboard transitions are paced at 20 ms.
Multitouch, cancellation, focus loss, hiding the viewport and disconnect clear
pending input. There is no click-on-touch-down behavior.

Only the local main window has access to the plugin. Native actions are scoped
by session ID. `TrackpadTest` covers gesture and US keyboard mapping behavior.

The WebRTC Android binary is pinned to `io.github.webrtc-sdk:android:144.7559.05`
(Maven Central, https://github.com/webrtc-sdk/android). It includes third-party
WebRTC components; retain the AAR's notices when distributing. The rest of this
plugin follows the repository's GPL-3.0-or-later license.
