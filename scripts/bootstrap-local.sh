#!/usr/bin/env bash
# macOS uses Homebrew; Arch/Omarchy extracts optional tools into .tools.
set -euo pipefail
source "$(dirname -- "$0")/env.sh"
cd "$KVM_PROJECT_ROOT"
if [[ "$KVM_HOST_OS" == Darwin ]]; then
  if ! command -v brew >/dev/null 2>&1; then
    printf 'Instala Homebrew desde https://brew.sh y vuelve a ejecutar este script.\n' >&2
    exit 1
  fi
  if ! xcode-select -p >/dev/null 2>&1; then
    printf 'Instala las herramientas de Apple con xcode-select --install.\n' >&2
    exit 1
  fi
  brew install pkgconf gstreamer libnice-gstreamer
elif [[ "$KVM_HOST_OS" != Linux ]]; then
  printf 'Plataforma no compatible: %s\n' "$KVM_HOST_OS" >&2
  exit 1
fi
mkdir -p .tools/downloads .tools/runtime
if ! command -v cargo >/dev/null; then
  curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o .tools/downloads/rustup.sh
  if [[ "$KVM_HOST_OS" == Darwin ]]; then
    sh .tools/downloads/rustup.sh -y --no-modify-path --profile minimal --default-toolchain stable
  else
    RUSTUP_HOME="$KVM_PROJECT_ROOT/.tools/rustup" CARGO_HOME="$KVM_PROJECT_ROOT/.tools/cargo" \
      sh .tools/downloads/rustup.sh -y --no-modify-path --profile minimal --default-toolchain stable
  fi
fi
source "$KVM_PROJECT_ROOT/scripts/env.sh"
if [[ "$KVM_HOST_OS" == Linux ]] && command -v pacman >/dev/null && command -v bsdtar >/dev/null; then
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
elif [[ "$KVM_HOST_OS" == Linux ]]; then
  printf 'Instala los plugins GStreamer con el gestor de paquetes de tu distribución.\n' >&2
fi
npm ci
bash scripts/doctor.sh
