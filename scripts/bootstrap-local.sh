#!/usr/bin/env bash
# Optional Arch/Omarchy bootstrap. Extracts tools into .tools; never uses sudo.
set -euo pipefail
source "$(dirname -- "$0")/env.sh"
cd "$KVM_PROJECT_ROOT"
mkdir -p .tools/downloads .tools/runtime
if ! command -v cargo >/dev/null; then
  curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o .tools/downloads/rustup.sh
  RUSTUP_HOME="$KVM_PROJECT_ROOT/.tools/rustup" CARGO_HOME="$KVM_PROJECT_ROOT/.tools/cargo" \
    sh .tools/downloads/rustup.sh -y --no-modify-path --profile minimal --default-toolchain stable
fi
if command -v pacman >/dev/null && command -v bsdtar >/dev/null; then
  pacman -Sp --print-format '%l' gst-plugins-good gst-plugins-bad gst-libav gst-plugin-gtk gst-plugin-va > .tools/downloads/plugins.urls
  while IFS= read -r package_url; do
    package_file="$KVM_PROJECT_ROOT/.tools/downloads/${package_url##*/}"
    if [[ "$package_url" == file://* ]]; then
      package_file="${package_url#file://}"
    elif [[ "$package_url" == https://* ]]; then
      curl --proto '=https' --tlsv1.2 -fLsS --retry 2 "$package_url" -o "$package_file"
    else
      printf 'URL no compatible: %s\n' "$package_url" >&2
      exit 1
    fi
    bsdtar -xf "$package_file" -C .tools/runtime
  done < .tools/downloads/plugins.urls
else
  printf 'Instala los plugins GStreamer con el gestor de paquetes de tu distribución.\n' >&2
fi
npm ci
bash scripts/doctor.sh
