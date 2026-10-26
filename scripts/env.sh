#!/usr/bin/env bash
# Source this file to use the project-local toolchain/runtime when present.
if [[ -n "${BASH_VERSION:-}" ]]; then
  KVM_ENV_FILE="${BASH_SOURCE[0]}"
elif [[ -n "${ZSH_VERSION:-}" ]]; then
  KVM_ENV_FILE="${(%):-%x}"
else
  printf 'Carga scripts/env.sh desde Bash o Zsh.\n' >&2
  return 1 2>/dev/null || exit 1
fi
KVM_PROJECT_ROOT="$(cd -- "$(dirname -- "$KVM_ENV_FILE")/.." && pwd)"
KVM_HOST_OS="$(uname -s)"
if [[ "$KVM_HOST_OS" == Linux && -x "$KVM_PROJECT_ROOT/.tools/cargo/bin/cargo" ]]; then
  export CARGO_HOME="$KVM_PROJECT_ROOT/.tools/cargo"
  export RUSTUP_HOME="$KVM_PROJECT_ROOT/.tools/rustup"
  export PATH="$CARGO_HOME/bin:$PATH"
fi
if [[ "$KVM_HOST_OS" == Linux && -d "$KVM_PROJECT_ROOT/.tools/runtime/usr/lib/gstreamer-1.0" ]]; then
  export LD_LIBRARY_PATH="$KVM_PROJECT_ROOT/.tools/runtime/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  export GST_PLUGIN_PATH="$KVM_PROJECT_ROOT/.tools/runtime/usr/lib/gstreamer-1.0${GST_PLUGIN_PATH:+:$GST_PLUGIN_PATH}"
fi
if [[ "$KVM_HOST_OS" == Darwin ]]; then
  if ! command -v brew >/dev/null 2>&1; then
    for KVM_BREW_BIN in /opt/homebrew/bin /usr/local/bin; do
      if [[ -x "$KVM_BREW_BIN/brew" ]]; then
        export PATH="$KVM_BREW_BIN:$PATH"
        break
      fi
    done
  fi
  if command -v brew >/dev/null 2>&1; then
    KVM_BREW_PREFIX="$(brew --prefix)"
    export PATH="$KVM_BREW_PREFIX/bin:$PATH"
    export PKG_CONFIG_PATH="$KVM_BREW_PREFIX/lib/pkgconfig:$KVM_BREW_PREFIX/share/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
    if KVM_NICE_PREFIX="$(brew --prefix libnice-gstreamer 2>/dev/null)" && [[ -d "$KVM_NICE_PREFIX/libexec/gstreamer-1.0" ]]; then
      export GST_PLUGIN_PATH="$KVM_NICE_PREFIX/libexec/gstreamer-1.0${GST_PLUGIN_PATH:+:$GST_PLUGIN_PATH}"
    fi
  fi
  if [[ -d "${HOME}/.cargo/bin" ]]; then
    export PATH="${HOME}/.cargo/bin:$PATH"
  fi
fi
