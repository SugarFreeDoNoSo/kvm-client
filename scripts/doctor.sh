#!/usr/bin/env bash
set -u
source "$(dirname -- "$0")/env.sh"
failed=0
for tool in cargo rustc node npm pkg-config gst-inspect-1.0; do
  if command -v "$tool" >/dev/null; then printf 'OK  %s\n' "$tool"; else printf 'FALTA  %s\n' "$tool"; failed=1; fi
done
for library in gtk+-3.0 webkit2gtk-4.1 gstreamer-1.0 gstreamer-webrtc-1.0 libei-1.0; do
  if pkg-config --exists "$library"; then printf 'OK  %s %s\n' "$library" "$(pkg-config --modversion "$library")"; else printf 'FALTA  %s\n' "$library"; failed=1; fi
done
for plugin in webrtcbin rtph264depay h264parse decodebin glupload glshader gtkglsink nicesrc; do
  if gst-inspect-1.0 "$plugin" >/dev/null 2>&1; then printf 'OK  %s\n' "$plugin"; else printf 'FALTA  %s\n' "$plugin"; failed=1; fi
done
if gst-inspect-1.0 vah264dec >/dev/null 2>&1 || gst-inspect-1.0 avdec_h264 >/dev/null 2>&1; then
  printf 'OK  decodificador H.264\n'
else printf 'FALTA  decodificador H.264\n'; failed=1; fi
printf 'Sesión: %s\n' "${XDG_SESSION_TYPE:-desconocida}"
exit "$failed"
