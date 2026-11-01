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

### Limitación de ráfagas de teclado

Pruebas deterministas añadidas: separación de 20 ms con mouse a 125/250/500/1000 Hz, orden de pulsación/liberación y modificadores, conservación del reporte tras fallo de transporte, eliminación de repeticiones redundantes y liberación de emergencia sin ejecutar teclas antiguas.

Pendiente en PicoKVM real: escritura rápida, combinaciones de modificadores, teclas mantenidas, escritura simultánea con movimiento y desconexión con teclas pulsadas. Confirmar pérdidas o duplicados y latencia antes de considerar los 50 reportes/s una frecuencia validada para el dispositivo.

### Recuperación de KeyUp en combinaciones Command de macOS

Pruebas añadidas para recuperar una liberación omitida una sola vez, conservar teclas físicamente pulsadas, evitar duplicar KeyUp normales y terminar Command+V con un reporte de teclado vacío. Pendiente verificar Windows+V y Windows+C con un teclado físico y PicoKVM, soltando primero V y luego Command y también en el orden inverso. Las pruebas simulan el estado físico; no demuestran la entrega de eventos de AppKit ni del firmware.

### Emparejamiento Sunshine: parámetros de URL

La prueba `pairing_url_encodes_device_name_without_injecting_parameters` verifica que el nombre `KVM Client` y nombres con espacios, Unicode y caracteres reservados llegan como un único parámetro, sin crear fragmentos ni alterar la solicitud. Una prueba HTTP local comprueba que la solicitud inicial llega al servidor y espera la respuesta. Las pruebas `cargo test --manifest-path src-tauri/Cargo.toml sunshine` pasaron, incluida la validación TLS con servidor local. Estas comprobaciones son sintéticas: queda pendiente pulsar Emparejar, confirmar una solicitud pendiente en Sunshine e introducir el PIN en un servidor real.

### Flujo de conexión Sunshine

El panel informa cuándo hace falta conectar PicoKVM y cuándo Sunshine no ofrece aplicaciones; identifica la etapa de los errores y permite restablecer el estado. La preparación del audio precede al inicio remoto de la aplicación. Pendiente de verificar con un equipo Sunshine real: selección de aplicación, arranque, video, audio, alternancia de fuentes y recuperación tras un fallo; las pruebas locales no demuestran la reproducción remota.

### Sunshine: autenticación HTTPS después del PIN

La configuración TLS de Sunshine desactiva la reanudación de sesión y mantiene la verificación exacta del certificado. Una prueba local con autenticación mutua confirma tres handshakes completos; con la configuración anterior falla al observar dos sesiones reanudadas. Se verificaron consultas consecutivas de información y aplicaciones contra el servidor emparejado con `saved_identity_authenticates_repeated_server_queries` (prueba ignorada por defecto; requiere `KVM_SUNSHINE_HOST` y `KVM_SUNSHINE_CONFIG_DIR`, puerto HTTP 47989). La identidad se conserva si falla solo la actualización posterior al emparejamiento. Esta validación no inicia una transmisión ni demuestra video, audio o entrada física.

### Movimiento durante la espera del teclado

Pruebas deterministas cubren movimiento a 1 ms con varias transiciones de teclado pendientes, orden del teclado a 20/40 ms, barreras de botones y rueda y conservación de deltas tras un envío fallido. El movimiento puede adelantarse a una tecla pendiente; los clics y la rueda conservan su orden. Pendiente validación física: apuntar mientras se mantiene WASD, disparar y mover el mouse, soltar teclas y repetir con cambios rápidos de dirección. No se ha medido latencia en Valorant ni el polling USB; los deltas AppKit actuales no son una ruta verificada de entrada sin aceleración.

### Perfiles de reescalado y resolución física

Perfiles disponibles: Original (bilineal, comparación), Escritorio (bicúbico con control de halos), Juegos 3D (adaptación SGSR v1) y Píxeles (vecino más cercano). Ajustar negocia salida a la región física con relación de aspecto conservada; las escalas manuales centran con offsets enteros, añaden márgenes o recortan. Se aplica el mismo ajuste a ambas fuentes nativas.

La prueba `kvm-smoke --synthetic --seconds 8` pasó en macOS con VideoToolbox: entrada 640×360, salida 1920×1080 en los tres perfiles; lienzo 1:1 de 1920×1200. Las fases registraron 60/75/75/75 cuadros nuevos presentados y cero descartes del sink, sin avisos de GStreamer en esa ejecución. Los tiempos de pared del filtro en las muestras finales fueron aproximadamente 0,16/0,10/0,15/0,22 ms; incluyen esperas y no son medidas de tiempo GPU, latencia de entrada o mejoras de FPS del juego. Los contadores de decodificación y presentación pueden diferir por arranque y la cola de último cuadro; cero descartes del sink no prueba cero descartes totales.

Las pruebas unitarias cubren ajuste de aspecto, PAR, viewport físico y deserialización de perfiles. Comparaciones PNG locales de texto, barras de color, damero y zona de frecuencias comprueban compilación GLSL y salida diferenciada; no miden estabilidad temporal, calidad perceptual ni reconstruyen detalle ausente en el video. Pendiente: GPU/sesión GTK de Linux, cambios entre pantallas con distinta escala y juegos reales con movimiento rápido. No se implementó generación de fotogramas ni mejora generativa; MetalFX exigiría una ruta Metal propia.

La comprobación `kvm-smoke --sunshine-self-test` también pasó: cuatro cambios de fuente; 120 cuadros decodificados y 119 presentados en cada motor, sin avisos de GStreamer, cero conexiones remotas y cero reportes HID. Esta prueba usa el perfil Escritorio compartido y no autentica un servidor Sunshine real.

La lectura GL a PNG verificó además Escritorio con nitidez 0 y viewport impar 1281×721: el área central de 640×360 coincide exactamente con Original a resolución nativa (921.600 canales RGBA comparados, cero diferencias). Esto comprueba alineación 1:1 en esa escena; no sustituye una evaluación perceptual de texto o juegos.

### Dashboard lateral, pantalla completa y persistencia

La vista previa se revisó a 1280×900: panel lateral de 360 píxeles, contenido desplazable y dashboard completo sin desbordamiento horizontal. La prueba nativa `kvm-smoke --dashboard-self-test` pasó en macOS: ventana de 960 puntos con panel oculto de 0 y video de 960, lateral de 360 y video de 600, pantalla completa del sistema con dashboard de 1728 y video de 0, restauración a lateral y ocultación con pestaña visible. No abrió conexiones remotas ni envió HID. GTK conserva una implementación equivalente, pendiente de sesión Linux real.

Las pruebas de preferencias verifican normalización de orígenes HTTPS, historial acotado, actualizaciones parciales, permisos 0600, ausencia de contraseñas en JSON y rechazo de archivos corruptos. El guardado de credenciales se ejecuta tras un login aceptado mediante HTTPS con certificado confiado; no al aceptar la solicitud de conexión en la interfaz. Las pruebas automatizadas no leen contraseñas del usuario. Keychain/Secret Service deben estar disponibles para recordar credenciales.

Las opciones de Moonlight se prueban contra los campos de la solicitud real: resolución/FPS/bitrate/red/audio del host; audio local desactivado usa fakesink y el volumen se verifica en el elemento de GStreamer. La oferta de valores no garantiza capacidad del host remoto.

### Pausa del video de PicoKVM sin cerrar el control

La prueba WebRTC sintética final pasó con el receptor de producción: tras 350 ms de drenaje, los contadores de decodificación y presentación permanecieron constantes durante 650 ms; una consulta RPC de solo lectura llegó al par remoto mientras el video estaba pausado; después de reanudar aparecieron cuadros nuevos. Se enviaron cero reportes HID y no se abrieron conexiones a dispositivos. No hubo avisos de GStreamer en esa ejecución. Esto demuestra la separación local del video y el control, no una reducción de trabajo del codificador del KVM ni una mejora medida en Valorant.

### Perfil EDID 720p

Se generaron `profiles/edid/kvm-720p60.bin` (256 bytes) y su equivalente hexadecimal. Se verificaron encabezado EDID 1.3, número de extensiones, ambos checksums, DTD 1280×720 a 74,25 MHz con totales 1650×750 (60 Hz), polaridades positivas y tamaño físico 320×180 mm. La extensión CTA anuncia VIC 4 como nativo y VIC 1 como respaldo VGA obligatorio, incluye HDMI VSDB y VCDB; los rangos cubren ambos modos. La validación de campos se revisó por separado del generador. No se ejecutó edid-decode ni se importó en el dispositivo. No se fija 1 FPS: el registro RPC público revisado de LuckfoxTECH/kvm no expone un setter de FPS; setVideoRc ajusta QP. Los límites de longitud aceptados por el componente nativo set_edid quedan pendientes de verificación física.

### Reescalado configurable y bloqueo de entrada

`cargo test --manifest-path src-tauri/Cargo.toml` pasó con 67 pruebas y una ignorada por requerir un host Sunshine. Las cinco pruebas de `enabled_tests` verifican liberaciones HID, descarte de eventos tardíos, rechazo de captura desactivada, reactivación sin residuos, invalidación nativa y reintentos del transporte. No se enviaron reportes a un dispositivo real.

`kvm-smoke --synthetic --seconds 12` pasó por Original, Escritorio, Juegos 3D, Escritorio 1:1, Píxeles y Píxeles 2×: entrada 640×360, salida ajustada 1920×1080 y lienzo manual 1920×1200, con cuadros nuevos en cada fase, cero descartes informados por el sink y cero avisos de GStreamer. RPC y pausa/reanudación siguieron funcionando. No mide latencia física ni rendimiento con juegos.

La lectura OpenGL a PNG en macOS compiló los cuatro shaders principales con tres patrones cada uno. Las cuatro variantes GLSL de compatibilidad también compilaron y coincidieron exactamente con las variantes core sobre el mismo patrón. Píxeles 2× reprodujo exactamente una entrada 17×11 sobre un lienzo impar 39×27, incluidos los márgenes. Al reducir un damero de un píxel de 511×255 a 139×73, el suavizado redujo la diferencia media entre canales rojo/verde de 129,82 a 2,06 en Escritorio y de 63,47 a 2,06 en Juegos 3D; es una medida de ese patrón, no una puntuación perceptual. Con suavizado, cambiar nitidez no alteró la salida. El control de halos mantuvo un escalón gris 64–192 dentro de ese rango (sin control: 55–201), y los umbrales SGSR 1 y 32 produjeron salidas distintas sobre un patrón de bajo contraste. Pendiente: evaluación visual en Linux y juegos reales.

### Paquete de distribución v0.1.0

La suite final pasó con 69 pruebas y una ignorada, incluyendo dos pruebas de resolución del runtime privado. Tauri produjo una aplicación arm64 con mínimo macOS 26.0, calculado también sobre las bibliotecas incorporadas: 66 Mach-O y 19 plugins, sin dependencias externas salvo las bibliotecas Apple. Se verificaron las firmas ad hoc con `codesign --verify --deep --strict`, el DMG con `hdiutil verify` y el ZIP extrayéndolo en un directorio nuevo. Desde esa extracción, `--check-runtime` pasó con un registro GStreamer nuevo y sin rutas multimedia heredadas: 25 elementos requeridos y decodificadores VideoToolbox disponibles; cero conexiones y reportes HID. No equivale a una instalación en otro Mac ni verifica Gatekeeper/notarización. La revisión visual nueva en navegador quedó incompleta por bloqueos de la herramienta; no se cuenta como aprobada.
