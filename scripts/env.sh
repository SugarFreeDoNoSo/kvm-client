#!/usr/bin/env bash
# Source this file to use the project-local toolchain/runtime when present.
KVM_PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -x "$KVM_PROJECT_ROOT/.tools/cargo/bin/cargo" ]]; then
  export CARGO_HOME="$KVM_PROJECT_ROOT/.tools/cargo"
  export RUSTUP_HOME="$KVM_PROJECT_ROOT/.tools/rustup"
  export PATH="$CARGO_HOME/bin:$PATH"
fi
if [[ -d "$KVM_PROJECT_ROOT/.tools/runtime/usr/lib/gstreamer-1.0" ]]; then
  export LD_LIBRARY_PATH="$KVM_PROJECT_ROOT/.tools/runtime/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  export GST_PLUGIN_PATH="$KVM_PROJECT_ROOT/.tools/runtime/usr/lib/gstreamer-1.0${GST_PLUGIN_PATH:+:$GST_PLUGIN_PATH}"
fi
