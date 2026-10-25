#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "$0")/env.sh"
cd "$KVM_PROJECT_ROOT"
exec npm run tauri -- dev "$@"
