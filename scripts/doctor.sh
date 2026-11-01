#!/usr/bin/env bash
set -u
source "$(dirname -- "$0")/env.sh"
failed=0
printf 'Plataforma: %s (%s)\n' "$KVM_HOST_OS" "$(uname -m)"
case "$KVM_HOST_OS" in
  Darwin) libraries=(gstreamer-1.0 gstreamer-video-1.0 gstreamer-gl-1.0 gstreamer-webrtc-1.0 gstreamer-sdp-1.0); sinks=(glimagesinkelement); decoders=(vtdec_hw vtdec avdec_h264 openh264dec) ;;
  Linux) libraries=(gtk+-3.0 webkit2gtk-4.1 gstreamer-1.0 gstreamer-video-1.0 gstreamer-webrtc-1.0 gstreamer-sdp-1.0 libei-1.0 dbus-1); sinks=(gtkglsink); decoders=(vah264dec vaapih264dec nvh264dec v4l2slh264dec avdec_h264 openh264dec) ;;
  *) printf 'Plataforma no compatible: %s\n' "$KVM_HOST_OS" >&2; exit 1 ;;
esac
for tool in cargo rustc node npm pkg-config gst-inspect-1.0; do
  if command -v "$tool" >/dev/null 2>&1; then
    printf 'OK  %s\n' "$tool"
  else
    printf 'FALTA  %s\n' "$tool"
    failed=1
  fi
done
if command -v rustc >/dev/null 2>&1; then
  rust_version="$(rustc --version)"
  if [[ "$rust_version" =~ rustc\ ([0-9]+)\.([0-9]+) ]] && (( BASH_REMATCH[1] > 1 || (BASH_REMATCH[1] == 1 && BASH_REMATCH[2] >= 92) )); then
    printf 'OK  %s (mínimo 1.92)\n' "$rust_version"
  else
    printf 'FALTA  Rust 1.92 o posterior (%s)\n' "$rust_version"
    failed=1
  fi
fi
if [[ "$KVM_HOST_OS" == Darwin ]]; then
  if xcode-select -p >/dev/null 2>&1; then
    printf 'OK  Xcode Command Line Tools\n'
  else
    printf 'FALTA  Xcode Command Line Tools (xcode-select --install)\n'
    failed=1
  fi
fi
if command -v pkg-config >/dev/null 2>&1; then
  for library in "${libraries[@]}"; do
    if [[ "$library" == gstreamer-* ]] && ! pkg-config --atleast-version=1.24 "$library"; then
      printf 'FALTA  %s >= 1.24\n' "$library"
      failed=1
    elif pkg-config --exists "$library"; then
      printf 'OK  %s %s\n' "$library" "$(pkg-config --modversion "$library")"
    else
      printf 'FALTA  %s\n' "$library"
      failed=1
    fi
  done
fi
if command -v gst-inspect-1.0 >/dev/null 2>&1; then
  for plugin in webrtcbin rtph264depay h264parse glupload glcolorconvert glshader capsfilter nicesrc dtlssrtpdec sctpenc "${sinks[@]}"; do
    if gst-inspect-1.0 "$plugin" >/dev/null 2>&1; then
      printf 'OK  %s\n' "$plugin"
    else
      printf 'FALTA  %s\n' "$plugin"
      failed=1
    fi
  done
  decoder_found=0
  for decoder in "${decoders[@]}"; do
    if gst-inspect-1.0 "$decoder" >/dev/null 2>&1; then
      printf 'OK  decodificador H.264 %s\n' "$decoder"
      decoder_found=1
      break
    fi
  done
  if [[ "$decoder_found" == 0 ]]; then
    printf 'FALTA  decodificador H.264\n'
    failed=1
  fi
  if ! gst-inspect-1.0 avdec_h264 >/dev/null 2>&1 && ! gst-inspect-1.0 openh264dec >/dev/null 2>&1; then
    printf 'FALTA  respaldo H.264 por software (avdec_h264 u openh264dec)\n'
    failed=1
  fi
  if gst-inspect-1.0 capssetter >/dev/null 2>&1; then
    printf 'OK  capssetter (prueba sintética)\n'
  else
    printf 'AVISO  capssetter ausente; kvm-smoke --synthetic no estará disponible\n'
  fi
  if gst-inspect-1.0 openh264enc >/dev/null 2>&1; then
    printf 'OK  openh264enc (prueba sintética)\n'
  elif gst-inspect-1.0 x264enc >/dev/null 2>&1; then
    printf 'OK  x264enc (prueba sintética)\n'
  else
    printf 'AVISO  openh264enc y x264enc ausentes; kvm-smoke --synthetic no estará disponible\n'
  fi
fi
if [[ "$KVM_HOST_OS" == Darwin ]]; then
  printf 'Entrada: macOS; la captura requiere que la ventana esté enfocada.\n'
else
  printf 'Sesión: %s\n' "${XDG_SESSION_TYPE:-desconocida}"
fi
exit "$failed"
