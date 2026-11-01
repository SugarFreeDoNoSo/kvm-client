Source: https://github.com/MrCreativ3001/moonlight-common-rust
Revision: a972937b2e84b21a6a390873930e3afe4cfa767d
License: GPL-3.0-or-later (see LICENSE).

Vendored to preserve Rust 1.92 compatibility: replace slice as_array/as_mut_array and VecDeque pop_front_if with equivalent stable helpers in src/compat.rs. The standalone manifest omits workspace tooling, examples and the unused C adapter dependency. The application enables http-client, std and rustcrypto only. No input calls are made by KVM Client.

Expose the URL builder via the http-client feature. KVM Client supplies its own HTTP transport with exact certificate pinning, and does not enable the upstream ureq transport.
