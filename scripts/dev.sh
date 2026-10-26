#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "$0")/env.sh"
cd "$KVM_PROJECT_ROOT"
bash scripts/doctor.sh
exec npm run tauri -- dev "$@"
