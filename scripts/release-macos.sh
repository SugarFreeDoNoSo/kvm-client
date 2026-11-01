#!/usr/bin/env bash
set -euo pipefail
source "$(dirname -- "$0")/env.sh"
cd "$KVM_PROJECT_ROOT"
if [[ "$(uname -s)" != Darwin ]]; then
  printf 'Esta build requiere macOS.\n' >&2
  exit 1
fi
if [[ $# -gt 1 || ( $# -eq 1 && "$1" != --skip-build ) ]]; then
  printf 'Uso: bash scripts/release-macos.sh [--skip-build]\n' >&2
  exit 1
fi
if [[ "${1:-}" != --skip-build ]]; then
  npm run tauri -- build --bundles app --no-sign --ci
fi
KVM_RELEASE_VERSION="$(node -p 'JSON.parse(require("fs").readFileSync("package.json", "utf8")).version')"
KVM_RELEASE_ARCH="$(uname -m)"
KVM_RELEASE_APP="$KVM_PROJECT_ROOT/src-tauri/target/release/bundle/macos/KVM Client.app"
KVM_RELEASE_OUTPUT="$KVM_PROJECT_ROOT/.tools/releases/v$KVM_RELEASE_VERSION"
KVM_RELEASE_NAME="kvm-client_${KVM_RELEASE_VERSION}_macos-${KVM_RELEASE_ARCH}"
mkdir -p "$KVM_RELEASE_OUTPUT"
python3 scripts/package-macos-runtime.py "$KVM_RELEASE_APP"

# Relocation invalidates existing signatures. Ad-hoc signing uses no private key.
python3 - "$KVM_RELEASE_APP" <<'PY'
from pathlib import Path
import subprocess
import sys
app = Path(sys.argv[1])
magic = {b'\xfe\xed\xfa\xce', b'\xce\xfa\xed\xfe', b'\xfe\xed\xfa\xcf',
         b'\xcf\xfa\xed\xfe', b'\xca\xfe\xba\xbe', b'\xbe\xba\xfe\xca'}
for path in sorted(app.rglob('*')):
    if path.is_file() and not path.is_symlink():
        with path.open('rb') as stream:
            is_macho = stream.read(4) in magic
        if is_macho:
            subprocess.run(['codesign', '--force', '--sign', '-', str(path)], check=True)
subprocess.run(['codesign', '--force', '--sign', '-', str(app)], check=True)
subprocess.run(['codesign', '--verify', '--deep', '--strict', str(app)], check=True)
PY
"$KVM_RELEASE_APP/Contents/MacOS/kvm-client" --check-runtime
ditto -c -k --sequesterRsrc --keepParent "$KVM_RELEASE_APP" "$KVM_RELEASE_OUTPUT/$KVM_RELEASE_NAME.app.zip"
KVM_RELEASE_STAGE="$(mktemp -d "$KVM_RELEASE_OUTPUT/dmg-XXXXXX")"
ditto "$KVM_RELEASE_APP" "$KVM_RELEASE_STAGE/KVM Client.app"
ln -s /Applications "$KVM_RELEASE_STAGE/Applications"
hdiutil create -volname 'KVM Client' -srcfolder "$KVM_RELEASE_STAGE" -ov -format UDZO "$KVM_RELEASE_OUTPUT/$KVM_RELEASE_NAME.dmg"
(
  cd "$KVM_RELEASE_OUTPUT"
  shasum -a 256 "$KVM_RELEASE_NAME.app.zip" "$KVM_RELEASE_NAME.dmg" > SHA256SUMS
)
printf 'Artefactos verificados: %s\n' "$KVM_RELEASE_OUTPUT"
