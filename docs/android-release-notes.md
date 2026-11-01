Android preview for Luckfox PicoKVM (Android 7.0+, ARM64).

- Panel button opens and closes the connection sidebar in portrait and landscape.
- Keyboard button opens the Android system keyboard; Esc, Tab, arrows, Backspace and Enter are available in the app.
- Native relative trackpad: dragging moves the pointer; a single tap does nothing; a quick double tap sends one left click.
- Native WebRTC video decoding and input. HTTPS/WSS retain explicit certificate fingerprint trust.

This is a debug-signed test APK, not a production or Play Store release. Physical Android/KVM behavior still requires device validation. Configure the remote keyboard layout as US; accented and other non-US characters are not supported yet. Sunshine and desktop image processing controls are not part of this first Android preview. No passwords are persisted.

Download `kvm-client-android-arm64-debug.apk` and check it against `SHA256SUMS`. Each CI run uses a temporary debug signing key, so installing over a previous preview may require uninstalling it (removing local certificate trust).
