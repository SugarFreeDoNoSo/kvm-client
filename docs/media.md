# Video nativo

El motor usa `webrtcbin` para SDP, ICE, DTLS/SRTP y el DataChannel fiable `rpc`.
Solo ofrece H.264 en la primera versión; no cambia el códec del KVM ni su firmware.
La sesión debe abrir primero la señalización y luego llamar `MediaEngine::start()`.
Los eventos `Offer` e `Ice` se envían al protocolo de señalización del equipo;
la respuesta SDP y los candidatos entrantes se entregan a `handle_answer` y `add_ice`.

La recepción utiliza:

```
webrtcbin → queue → rtph264depay → h264parse → decoder
          → queue (un cuadro, leaky downstream)
          → glupload → glcolorconvert → glshader → gtkglsink
```

No se descartan paquetes RTP deliberadamente. La cola limitada descarta cuadros
ya decodificados antiguos cuando la GPU no sigue el ritmo. El jitter buffer es
de 10 ms y el sink no espera un segundo reloj de presentación. Esto no elimina
el retardo del codificador, red, monitor o del host remoto.

Se buscan decodificadores VA, VAAPI, NVIDIA y V4L2 antes de `avdec_h264` y
`openh264dec`. Si un decodificador hardware informa un error, se reemplaza por
software conservando el receptor RTP y la superficie GTK. El nombre real se
publica mediante el evento `Decoder`.

El filtro inicial conserva la interpolación espacial bilineal de la textura.
La opción de nitidez aplica una máscara espacial de cinco muestras, limitada al
rango local para reducir halos; no genera cuadros ni conserva cuadros anteriores.
Los filtros se ejecutan en la GPU y no pasan imágenes por JavaScript.
El filtro proporciona etapas de vértices y fragmentos compatibles entre sí;
dejar que GStreamer genere solo la etapa de vértices puede mezclar versiones GLSL
cuando la ventana comparte el contexto OpenGL con WebKit. Al cambiar los shaders,
validar también un contexto OpenGL 3 de Mesa, además de la GPU física.

## GTK y propiedad de objetos

Construir el motor y obtener `widget_ptr()` en el hilo principal GTK. El puntero
es prestado y pertenece a `gtkglsink`; la vista debe crear su propia referencia
`gtk::Widget` con `from_glib_none` y quitarla al desconectar. El resto del motor
usa objetos GStreamer aptos para varios hilos. `shutdown()` cierra el canal y
lleva el pipeline a `NULL`; debe llamarse antes de soltar el motor.

## Medidas

- FPS recibidos: unidades de acceso H.264 que salen del parser, no paquetes RTP.
- FPS decodificados: cuadros que salen del decodificador.
- FPS presentados: diferencia del contador `rendered` del sink; representa cuadros
  entregados al sink, no una medición externa del scanout del monitor.
- RTT y paquetes perdidos: estadísticas reales de `webrtcbin`; cuando no hay RTT
  disponible se mide un `ping` JSON-RPC cada dos segundos. Ese respaldo incluye el
  procesamiento del servidor. La ausencia de datos se representa como `null`.
- La latencia extremo a extremo solo se valida con medición visual externa.

`send_rpc` rechaza tráfico cuando el buffer SCTP supera 32 KiB. El planificador
de entrada conserva los deltas para reintentar; el motor no añade una segunda
cola ilimitada.

Dependencias de ejecución: GStreamer base/good/bad, el plugin GTK, libnice y un
decodificador H.264 (por ejemplo gst-libav). El sistema puede distribuir GTK,
WebRTC, VA y libnice en paquetes separados.

## Prueba con el equipo real

El binario `kvm-smoke` abre una ventana GTK independiente y usa los mismos módulos
de sesión, TLS, señalización y video de la aplicación. Requiere que se proporcione
la huella SHA-256 completa del equipo:

```sh
cargo run --manifest-path src-tauri/Cargo.toml --bin kvm-smoke -- \
  --address https://192.168.100.8/ --fingerprint 'AA:BB:…' --seconds 10
```

`AA:BB:…` es un ejemplo: debe sustituirse por los 32 bytes de una huella comprobada.
Usa un archivo de confianza temporal que elimina al terminar; no acepta
automáticamente el certificado observado ni cambia la confianza de la aplicación.
Si el equipo tiene contraseña, se puede proporcionar mediante `KVM_PASSWORD`.
La prueba no captura entrada ni envía reportes HID. Solo aprueba si el canal RPC
abre y aparecen cuadros decodificados y presentados; publica las métricas reales
en JSON y sale con código distinto de cero si falla.

Cuando no hay una señal HDMI disponible, se puede validar la ruta completa del
receptor con un emisor local, sin conectar a ningún KVM:

```sh
cargo run --manifest-path src-tauri/Cargo.toml --bin kvm-smoke -- \
  --synthetic --seconds 8
```

Este modo requiere `openh264enc`. Genera una escena local, la codifica en H.264 y
la transmite entre dos pares WebRTC reales con ICE/DTLS/SRTP y un DataChannel.
El receptor es el mismo `MediaEngine` de la aplicación. A mitad de la prueba se
activa la nitidez espacial y se verifica que la decodificación y la presentación
siguen activas. Aprobar esta prueba no sustituye la prueba con HDMI del equipo.

El acceso al display y a la GPU debe estar disponible para esa ejecución. Un
entorno aislado sin acceso gráfico puede fallar con `EGL_NOT_INITIALIZED` aunque
las dependencias estén instaladas.
