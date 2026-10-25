# Repository Guidelines

## Project Structure & Module Organization

This Linux desktop client connects to Luckfox PicoKVM using Tauri 2, React, TypeScript, and Rust/GStreamer. `src/` contains the interface (`App.tsx`), native API bridge (`backend.ts`), and CSS. `src-tauri/src/` contains networking, signaling, media, and input modules; `input/` includes the scheduler and Wayland/X11 backends. Shaders live in `src-tauri/src/shaders/`, application icons in `src-tauri/icons/`, and the diagnostic executable in `src-tauri/src/bin/kvm-smoke.rs`. See `docs/` for architecture and verification notes. `scripts/` supplies environment and development helpers.

## Build, Test, and Development Commands

- `npm ci`: install the locked frontend dependencies.
- `npm run doctor`: check native libraries and GStreamer plugins.
- `npm run desktop`: launch the Tauri development application with the project environment.
- `npm run dev`: serve the frontend alone through Vite.
- `npm run check`: run strict TypeScript checks.
- `npm run build`: type-check and build the frontend into `dist/`.
- `npm run native:check`: build the frontend and run Rust tests through `scripts/check.sh`.
- `source scripts/env.sh`, then `npm run tauri -- build --no-bundle`: build the production executable.
- `cargo fmt --manifest-path src-tauri/Cargo.toml --check`: check Rust formatting.

Native development requires Rust 1.92 or later, GTK3, WebKitGTK 4.1, GStreamer, libnice, and libei; consult `README.md` for setup.

## Coding Style & Naming Conventions

Use two-space indentation in TypeScript/TSX and standard rustfmt formatting in Rust. Name React components and TypeScript types in PascalCase, functions and variables in camelCase, and Rust functions/modules in snake_case. Keep TypeScript strict and free of unused locals and parameters. Preserve the native media/input architecture: video and frequent mouse events stay outside JavaScript. No dedicated frontend formatter or linter is configured.

## Testing Guidelines

Use Rust's built-in test framework, with descriptive snake_case test names alongside the relevant modules. Cover scheduler ordering, motion preservation, transport failures, signaling, and TLS trust when changing those paths. No coverage threshold or frontend test runner is configured. Document hardware-dependent checks and measurement limits in `docs/verification.md`; distinguish synthetic WebRTC checks from physical input validation.

## Commit & Pull Request Guidelines

The repository has no commits yet. Use concise, imperative commit subjects and keep changes focused. Pull requests should explain the behavior changed, list validation commands and results, link relevant issues, and include screenshots for interface changes. Identify any untested hardware or desktop-session behavior.

## Security & Configuration

Preserve explicit certificate-fingerprint trust. Never commit passwords, cookies, `.env`, or generated build artifacts. Firmware and EDID changes are outside the current client scope.
