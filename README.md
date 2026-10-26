# KVM Client

Consola de escritorio para Luckfox PicoKVM, con Tauri 2, React y un motor nativo Rust/GStreamer. Conecta directamente a las APIs del dispositivo; el video y los eventos frecuentes del mouse no pasan por JavaScript.

Plataformas: macOS y Linux (Wayland/Hyprland o X11). Dirección inicial: `https://192.168.100.8/`. La ejecución actual es desde el repositorio; no se distribuye una aplicación autónoma con GStreamer incluido.

## Ejecutar

```bash
npm ci
bash scripts/doctor.sh
bash scripts/dev.sh
```

Se necesitan Rust 1.92 o posterior, Node compatible con Vite 7 y las bibliotecas nativas de la plataforma. `npm run doctor` selecciona los requisitos de macOS o Linux y devuelve un error si falta un componente obligatorio.

### macOS

Instala Xcode Command Line Tools (`xcode-select --install`), [Rust](https://rustup.rs) y [Homebrew](https://brew.sh). Después:

```bash
brew install pkgconf gstreamer libnice-gstreamer
npm ci
npm run doctor
npm run desktop
```

GStreamer 1.24 o posterior debe incluir WebRTC, libnice, OpenGL y un decodificador H.264. Se prefiere VideoToolbox (`vtdec_hw` o `vtdec`) y se conserva un respaldo por software (`avdec_h264` u `openh264dec`). La prueba sintética requiere `capssetter` y un codificador `openh264enc` o `x264enc`. La vista nativa usa AppKit; no se necesitan GTK, WebKitGTK, libei ni X11 en macOS. El video permanece fuera del webview.

`bash scripts/bootstrap-local.sh` también prepara las dependencias: en macOS instala `pkgconf`, `gstreamer` y `libnice-gstreamer` mediante Homebrew, y Rust si está ausente. Estas instalaciones son globales al usuario/Homebrew. `scripts/env.sh` selecciona Homebrew, añade el plugin ICE de `libnice-gstreamer` y no carga el runtime Linux de `.tools/`. Instalar únicamente `libnice` no proporciona el elemento `nicesrc` que requiere WebRTC.

### Linux

Se necesitan GTK3, WebKitGTK 4.1, GStreamer con WebRTC/OpenGL/GTK/H.264, libnice y libei. En Arch/Omarchy:

```bash
sudo pacman -S --needed rust webkit2gtk gtk3 gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-libav gst-plugin-gtk gst-plugin-va libnice libei
```

En Arch/Omarchy también se puede preparar un entorno local sin administrador con `bash scripts/bootstrap-local.sh`. Descarga Rust y extrae los paquetes GStreamer faltantes en `.tools/`, sin instalación global. `scripts/env.sh` selecciona ese entorno en Linux cuando existe.

`npm run dev` abre solo la interfaz web de desarrollo. Para recibir video y controlar el equipo hay que ejecutar la aplicación de escritorio con `bash scripts/dev.sh`.

## Uso

1. Introducir la dirección del KVM y su contraseña local, si está habilitada.
2. Conectar y revisar la huella SHA-256 del certificado en el primer acceso. Un cambio de certificado exige aceptar la nueva huella; no se envían credenciales durante la inspección.
3. Activar **Capturar control**. En macOS se captura únicamente la ventana enfocada; `Ctrl+Option+Esc` libera el control. En Wayland se autoriza el portal y después se cruza el borde derecho de la pantalla; se puede cancelar mientras espera. En Linux el atajo es `Ctrl+Alt+Esc`. Perder el foco o desconectar libera el control.
4. Ajustar sensibilidad y frecuencia. Se conserva la distancia del movimiento y se dividen deltas grandes en reportes HID válidos. El envío tiene un reloj independiente del refresco de video.
5. Seleccionar reescalado bilineal o nitidez espacial. El filtro no necesita cuadros anteriores ni genera cuadros artificiales.

Si el KVM informa **Sin señal HDMI** después de bloquearse o suspenderse la pantalla del equipo remoto, captura el control y pulsa Shift para despertarla.

La interfaz muestra la fase de conexión (TLS, autenticación, señalización y negociación WebRTC) y el estado del video por separado. Un canal conectado puede estar esperando cuadros, sin señal HDMI o con el video detenido. Los errores nativos de conexión tienen prioridad sobre mensajes anteriores de ajustes.

Si no inicia, ejecuta primero `npm run doctor` y corrige los componentes marcados
como `FALTA`. Ejecutar `npm run desktop` desde una terminal conserva los mensajes
`[kvm]` de estado, fase y video para el diagnóstico. `npm run dev` es únicamente
la vista previa; no recibe video ni captura entrada.

Los Hz mostrados son reportes enviados por el cliente, no una medida de la frecuencia USB del KVM. Los FPS recibidos, decodificados y presentados se muestran por separado; el contador de presentación procede del sink de video y no mide el barrido físico del monitor. RTT tampoco equivale a latencia de extremo a extremo.

El firmware debe transmitir H.264 (`avc`). Esta versión detecta incompatibilidades y no cambia automáticamente códec, EDID ni firmware. No incluye energía, medios virtuales, audio reproducido ni generación de cuadros.

## Comprobar y compilar

```bash
bash scripts/check.sh
source scripts/env.sh
npm run build
cargo build --manifest-path src-tauri/Cargo.toml --features custom-protocol
bash scripts/start.sh
```

El ejecutable queda en `src-tauri/target/debug/kvm-client` e incorpora la interfaz de producción. `scripts/start.sh` selecciona el entorno de la plataforma y verifica las dependencias. Para compilar una versión optimizada:

```bash
source scripts/env.sh
npm run tauri -- build --no-bundle
```

Las pruebas del planificador verifican conservación de deltas, sensibilidad fraccional, orden de arrastres, retención ante errores de envío, límites de cola y liberación. También hay pruebas del protocolo de señalización y de la confianza TLS.

Conserva `Cargo.lock` para reproducir las versiones compatibles con Rust 1.92.
`kstring` se fija a 2.0.2 porque la actualización 2.0.5 exige Rust 1.96.
Las pruebas reproducibles pueden ejecutarse con
`cargo test --manifest-path src-tauri/Cargo.toml --features custom-protocol --locked`.

Las mediciones y los límites de las pruebas realizadas están en [docs/verification.md](docs/verification.md).

## Diseño

- HTTPS/WSS autenticados por huella exacta del certificado; cookies y contraseña permanecen en memoria. Solo se persisten las huellas por origen en el directorio de configuración de la aplicación.
- Señalización `/webrtc/signaling/client`; JSON-RPC sobre DataChannel `rpc`. `/api/rpc` se reserva para consultas de diagnóstico con `X-Session-ID`.
- H.264 con decodificador por hardware disponible y respaldo por software, cola de render de un cuadro y shaders espaciales. La salida OpenGL se integra en la ventana Tauri mediante AppKit/GstVideoOverlay en macOS o GTK en Linux.
- Entrada nativa limitada a la ventana enfocada en macOS; portal InputCapture/libei en Wayland y XInput2 en X11. Más detalles en [docs/input.md](docs/input.md) y [docs/media.md](docs/media.md).

Referencias del protocolo: [código público de Luckfox](https://github.com/LuckfoxTECH/kvm), [GStreamer WebRTC](https://gstreamer.freedesktop.org/documentation/webrtc/index.html), [portal InputCapture](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.InputCapture.html).

## Medición y siguientes etapas

Comparar con la interfaz original bajo la misma resolución y escena. Registrar FPS, pérdidas, regularidad de envío, CPU/GPU y latencia visual con una medición externa. Subir de 125 a 250/500/1000 Hz únicamente mientras el canal permanezca estable; no se promete una frecuencia USB ni FPS superiores a la fuente.

Las modificaciones de firmware quedan para una etapa posterior, basada en mediciones y con respaldo y recuperación antes de escribir en el dispositivo. Este proyecto no flashea ni modifica el firmware.
