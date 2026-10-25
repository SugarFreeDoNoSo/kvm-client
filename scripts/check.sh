#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "$0")/env.sh"
cd "$KVM_PROJECT_ROOT"
npm run build
cargo test --manifest-path src-tauri/Cargo.toml "$@"
