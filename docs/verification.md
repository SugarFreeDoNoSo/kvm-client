# Verificación

## Revisión multiplataforma actual

La revisión añade la ruta AppKit/OpenGL y captura nativa de la ventana enfocada
para macOS, conservando la ruta GTK en Linux. Las pruebas de compilación, el
diagnóstico sintético y la entrada física se registran por separado. Aprobar
pruebas unitarias o sintéticas no valida una conexión PicoKVM real en macOS.

Comprobaciones realizadas en esta revisión (macOS arm64, Rust 1.92.0,
GStreamer 1.28.8 de Homebrew):

- `npm run check`: aprobado (TypeScript estricto).
- `npm run build`: aprobado (TypeScript y bundle Vite).
- `bash -n scripts/*.sh`: aprobado.
- `scripts/env.sh` cargado desde Bash y Zsh: raíz del proyecto y selección Darwin correctas.
- `npm run doctor`: aprobado, incluidos `nicesrc` mediante `libnice-gstreamer`, `glimagesinkelement`, `vtdec_hw`, respaldo software y `x264enc` para la prueba sintética.
- `cargo check --all-targets --manifest-path src-tauri/Cargo.toml`: aprobado en macOS.
- `cargo test --features custom-protocol --locked --manifest-path src-tauri/Cargo.toml`: 32 pruebas aprobadas, sin omitidas, en macOS.
- `npm run tauri -- build --debug --no-bundle`: aprobado en macOS.
- Arranque del ejecutable macOS durante cuatro segundos, sin conexión ni captura: permaneció activo sin pánicos ni errores en stderr; se terminó desde la prueba.

El diagnóstico local macOS `kvm-smoke --input-self-test` pasó captura,
cancelación, pérdida de foco y restauración del cursor mediante AppKit real,
con un receptor HID simulado y cero conexiones a un KVM. No demuestra movimiento
ni escritura físicos en el equipo remoto.

El video sintético macOS pasó 30 segundos con ambos decodificadores:

| Ruta | Decodificador | Cuadros decodificados | Cuadros presentados |
| --- | --- | ---: | ---: |
| Hardware | `vtdec_hw` | 915 | 911 |
| Software (`KVM_FORCE_SOFTWARE=1`) | `avdec_h264` | 915 | 913 |

Ambas ejecuciones mantuvieron aproximadamente 30 FPS, abrieron RPC, cambiaron a
nitidez durante la reproducción, enviaron cero reportes HID y terminaron con
código cero. Estos contadores corresponden al pipeline y al sink; no miden el
barrido físico ni la latencia extremo a extremo.

La primera ejecución sintética abrió RPC, pero no produjo cuadros por una
comparación literal de flags de perfiles Constrained Baseline equivalentes
(`42c0` de x264 y `42e0` en la oferta). El emisor de diagnóstico normaliza esas
caps tras validar el SPS; conserva la oferta del receptor de producción. El
diagnóstico también propaga el resultado al código de salida del proceso, en
lugar de finalizar siempre con cero desde Tauri.

Regresión Linux de esta revisión, en contenedor ARM Debian Trixie:

- `cargo check --all-targets`: aprobado.
- `cargo test`: 26 pruebas aprobadas y una omitida porque requiere un escritorio interactivo.
- `cargo build --features custom-protocol --bins`: aprobado; se limitó a un trabajo por la memoria del contenedor.
- Video sintético de 30 segundos bajo Xvfb y decodificación software: RPC abierto, 917 cuadros decodificados y 910 presentados, aproximadamente 30 FPS, cambio a nitidez aprobado, cero reportes HID y código de salida cero.

Para repetir la validación macOS, desde una sesión gráfica:

```bash
npm ci
npm run doctor
npm run native:check
source scripts/env.sh
cargo run --manifest-path src-tauri/Cargo.toml --bin kvm-smoke -- --synthetic --seconds 8
cargo run --manifest-path src-tauri/Cargo.toml --bin kvm-smoke -- --input-self-test
npm run desktop
```

Quedan pendientes Retina/pantalla completa, reconexión y cierre interactivo de
la aplicación completa. La ejecución sintética no sustituye verificar con un
PicoKVM la confianza TLS explícita, autenticación, HDMI disponible y ausente,
captura física de mouse/teclado, `Ctrl+Option+Esc` y liberación al perder foco.
No se ha validado una sesión PicoKVM real en macOS en esta revisión. Las tasas
USB y la latencia extremo a extremo requieren medición externa.

## Registro histórico: primera versión Linux

Fecha: 25 de octubre de 2026. Equipo cliente: Omarchy, Wayland/Hyprland, Intel Iris Xe, WebKitGTK 2.54.1 y GStreamer 1.28.8. Dependencias de compilación y plugins faltantes preparados dentro de `.tools/`, sin instalación global.

### Comprobaciones realizadas

- `npm run build`: TypeScript estricto y compilación Vite aprobados.
- `cargo fmt --check`: aprobado.
- `cargo test`: 17 pruebas principales aprobadas; otras ocho vuelven a comprobar los módulos compartidos desde el ejecutable de diagnóstico.
- `cargo build --features custom-protocol --bins`: genera la aplicación con interfaz incorporada y el diagnóstico `kvm-smoke`.
- `npm run doctor`: bibliotecas y plugins necesarios disponibles.

### Transmisión real

Dispositivo Luckfox en `https://192.168.100.8/`, configurado sin contraseña. La sesión HTTPS/WSS usó una huella SHA-256 explícita; las pruebas de interfaz guardaron esa confianza en configuración temporal que se eliminó al terminar.

Inicialmente el dispositivo informó `no_signal`, 0×0 y 0 FPS. Tras la indicación del usuario, se envió una única pulsación breve de Shift y su liberación por la API. La entrada HDMI pasó a 1920×1080 a 60 Hz.

La prueba real de WebRTC abrió el DataChannel RPC y recibió, decodificó y presentó aproximadamente 60 FPS usando `vah264dec`. El diagnóstico no envió reportes HID. La medición corresponde a contadores del pipeline, no a latencia visual externa ni al barrido físico del monitor.

### Interfaz y GPU

La aplicación Tauri/GTK se verificó en una pantalla Xvfb aislada, sin desbloquear ni alterar la sesión del usuario. Pasaron el diálogo de certificado, conexión real, cambio de bilineal a nitidez durante reproducción, desconexión y reconexión sin repetir la confianza ya aceptada en esa sesión temporal.

En Xvfb se observaron 60 FPS recibidos y decodificados, con 32–48 FPS entregados al sink por el renderizado virtual. El RTT RPC observado estuvo entre 5,5 y 8,7 ms. Estos valores no representan el rendimiento gráfico de la pantalla física ni la latencia total de interacción.

La prueba detectó y permitió corregir una incompatibilidad de versiones GLSL entre el fragmento propio y el vertex shader automático. Ahora se suministran ambos stages. La corrección pasó en Mesa/OpenGL 3 y en la GPU física.

El modo `kvm-smoke --synthetic` también pasó: H.264 local transmitido mediante dos pares WebRTC reales, decodificado por hardware y presentado a aproximadamente 30 FPS, con cambio a nitidez durante la reproducción. No se conecta a un KVM ni envía HID.

### Entrada y límites de la validación

Las pruebas cubren conservación de distancia, deltas firmados y fraccionales, dirección diagonal, orden de botones/rueda, reintento de transporte, límite de cola, liberación de modificadores y recuperación de indicadores tras errores mientras se está desconectado.

La captura física mediante el portal Wayland y la tasa efectiva de reportes USB requieren una sesión interactiva. No se midió el polling USB ni se comparó la latencia extremo a extremo con el cliente original. Los valores 125/250/500/1000 Hz son objetivos del planificador del cliente.

No se modificó firmware, códec, EDID ni configuración del escritorio. Las ventanas, servidores gráficos y archivos de confianza creados para las pruebas se cerraron o eliminaron al terminar.

### Pantalla completa sin barra

El botón de pantalla completa elimina el espacio nativo de la barra (GTK y AppKit). Al liberar una captura activa con Ctrl+Alt+Esc (Linux) o Ctrl+Option+Esc (macOS), la aplicación sale de pantalla completa y recupera los controles. También se puede salir con los controles de pantalla completa del sistema; el cambio de tamaño sincroniza la barra y el botón.

Validación pendiente con una sesión remota real: comprobar que el video ocupa todo el espacio, que el atajo recupera los controles y que entrar/salir repetidamente conserva la captura y el tamaño de la imagen. La compilación en macOS no valida la ruta GTK de Linux ni la interacción física del teclado y mouse.
