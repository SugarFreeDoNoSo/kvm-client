#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "$0")/env.sh"
cd "$KVM_PROJECT_ROOT"
bash scripts/doctor.sh
if [[ ! -x src-tauri/target/debug/kvm-client ]]; then
  printf 'Compila primero: npm run build && cargo build --manifest-path src-tauri/Cargo.toml --features custom-protocol\n' >&2
  exit 1
fi
exec src-tauri/target/debug/kvm-client "$@"
