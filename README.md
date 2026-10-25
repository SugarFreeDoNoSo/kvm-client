# KVM Client

Consola de escritorio para Luckfox PicoKVM, con Tauri 2, React y un motor nativo Rust/GStreamer. Conecta directamente a las APIs del dispositivo; el video y los eventos frecuentes del mouse no pasan por JavaScript.

Primera plataforma: Linux, con Wayland/Hyprland y backend X11 opcional. Dirección inicial: `https://192.168.100.8/`.

## Ejecutar

```bash
npm ci
bash scripts/doctor.sh
bash scripts/dev.sh
```

El equipo necesita Rust estable, Node, GTK3, WebKitGTK 4.1, GStreamer con WebRTC/OpenGL/GTK/H.264, libnice y libei. En Arch/Omarchy:

```bash
sudo pacman -S --needed rust webkit2gtk gtk3 gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-libav gst-plugin-gtk gst-plugin-va libnice libei
```

También se puede preparar un entorno local sin administrador con `bash scripts/bootstrap-local.sh`. Este descarga Rust y extrae los paquetes GStreamer faltantes en `.tools/`; no instala paquetes globalmente. `scripts/env.sh` selecciona ese entorno cuando existe.

`npm run dev` abre solo la interfaz web de desarrollo. Para recibir video y controlar el equipo hay que ejecutar la aplicación de escritorio con `bash scripts/dev.sh`.

## Uso

1. Introducir la dirección del KVM y su contraseña local, si está habilitada.
2. Conectar y revisar la huella SHA-256 del certificado en el primer acceso. Un cambio de certificado exige aceptar la nueva huella; no se envían credenciales durante la inspección.
3. Activar **Capturar control**. En Wayland se abre el portal de captura y después se cruza el borde derecho de la pantalla para activarla. Se puede cancelar mientras espera. `Ctrl+Alt+Esc` libera el control; también se libera al perder el foco o desconectar.
4. Ajustar sensibilidad y frecuencia. Se conserva la distancia del movimiento y se dividen deltas grandes en reportes HID válidos. El envío tiene un reloj independiente del refresco de video.
5. Seleccionar reescalado bilineal o nitidez espacial. El filtro no necesita cuadros anteriores ni genera cuadros artificiales.

Si el KVM informa **Sin señal HDMI** después de bloquearse o suspenderse la pantalla del equipo remoto, captura el control y pulsa Shift para despertarla.

Los Hz mostrados son reportes enviados por el cliente, no una medida de la frecuencia USB del KVM. Los FPS recibidos, decodificados y presentados se muestran por separado; el contador de presentación procede del sink de video y no mide el barrido físico del monitor. RTT tampoco equivale a latencia de extremo a extremo.

El firmware debe transmitir H.264 (`avc`). Esta versión detecta incompatibilidades y no cambia automáticamente códec, EDID ni firmware. No incluye energía, medios virtuales, audio reproducido ni generación de cuadros.

## Comprobar y compilar

```bash
bash scripts/check.sh
source scripts/env.sh
cargo build --manifest-path src-tauri/Cargo.toml --features custom-protocol
bash scripts/start.sh
```

El ejecutable queda en `src-tauri/target/debug/kvm-client` e incorpora la interfaz de producción. `scripts/start.sh` selecciona también las bibliotecas locales. Para compilar una versión optimizada:

```bash
source scripts/env.sh
npm run tauri -- build --no-bundle
```

Las pruebas del planificador verifican conservación de deltas, sensibilidad fraccional, orden de arrastres, retención ante errores de envío, límites de cola y liberación. También hay pruebas del protocolo de señalización y de la confianza TLS.

Las mediciones y los límites de las pruebas realizadas están en [docs/verification.md](docs/verification.md).

## Diseño

- HTTPS/WSS autenticados por huella exacta del certificado; cookies y contraseña permanecen en memoria. Solo se persisten las huellas por origen en el directorio de configuración de la aplicación.
- Señalización `/webrtc/signaling/client`; JSON-RPC sobre DataChannel `rpc`. `/api/rpc` se reserva para consultas de diagnóstico con `X-Session-ID`.
- H.264 con decodificador por hardware disponible y respaldo por software, cola de render de un cuadro, shaders espaciales y salida GTK/OpenGL en la ventana Tauri.
- Entrada mediante portal InputCapture/libei en Wayland y XInput2 en X11. Más detalles en [docs/input.md](docs/input.md) y [docs/media.md](docs/media.md).

Referencias del protocolo: [código público de Luckfox](https://github.com/LuckfoxTECH/kvm), [GStreamer WebRTC](https://gstreamer.freedesktop.org/documentation/webrtc/index.html), [portal InputCapture](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.InputCapture.html).

## Medición y siguientes etapas

Comparar con la interfaz original bajo la misma resolución y escena. Registrar FPS, pérdidas, regularidad de envío, CPU/GPU y latencia visual con una medición externa. Subir de 125 a 250/500/1000 Hz únicamente mientras el canal permanezca estable; no se promete una frecuencia USB ni FPS superiores a la fuente.

Las modificaciones de firmware quedan para una etapa posterior, basada en mediciones y con respaldo y recuperación antes de escribir en el dispositivo. Este proyecto no flashea ni modifica el firmware.
