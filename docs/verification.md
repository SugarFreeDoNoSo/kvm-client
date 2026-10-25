# Verificación de la primera versión

Fecha: 25 de octubre de 2026. Equipo cliente: Omarchy, Wayland/Hyprland, Intel Iris Xe, WebKitGTK 2.54.1 y GStreamer 1.28.8. Dependencias de compilación y plugins faltantes preparados dentro de `.tools/`, sin instalación global.

## Comprobaciones realizadas

- `npm run build`: TypeScript estricto y compilación Vite aprobados.
- `cargo fmt --check`: aprobado.
- `cargo test`: 17 pruebas principales aprobadas; otras ocho vuelven a comprobar los módulos compartidos desde el ejecutable de diagnóstico.
- `cargo build --features custom-protocol --bins`: genera la aplicación con interfaz incorporada y el diagnóstico `kvm-smoke`.
- `npm run doctor`: bibliotecas y plugins necesarios disponibles.

## Transmisión real

Dispositivo Luckfox en `https://192.168.100.8/`, configurado sin contraseña. La sesión HTTPS/WSS usó una huella SHA-256 explícita; las pruebas de interfaz guardaron esa confianza en configuración temporal que se eliminó al terminar.

Inicialmente el dispositivo informó `no_signal`, 0×0 y 0 FPS. Tras la indicación del usuario, se envió una única pulsación breve de Shift y su liberación por la API. La entrada HDMI pasó a 1920×1080 a 60 Hz.

La prueba real de WebRTC abrió el DataChannel RPC y recibió, decodificó y presentó aproximadamente 60 FPS usando `vah264dec`. El diagnóstico no envió reportes HID. La medición corresponde a contadores del pipeline, no a latencia visual externa ni al barrido físico del monitor.

## Interfaz y GPU

La aplicación Tauri/GTK se verificó en una pantalla Xvfb aislada, sin desbloquear ni alterar la sesión del usuario. Pasaron el diálogo de certificado, conexión real, cambio de bilineal a nitidez durante reproducción, desconexión y reconexión sin repetir la confianza ya aceptada en esa sesión temporal.

En Xvfb se observaron 60 FPS recibidos y decodificados, con 32–48 FPS entregados al sink por el renderizado virtual. El RTT RPC observado estuvo entre 5,5 y 8,7 ms. Estos valores no representan el rendimiento gráfico de la pantalla física ni la latencia total de interacción.

La prueba detectó y permitió corregir una incompatibilidad de versiones GLSL entre el fragmento propio y el vertex shader automático. Ahora se suministran ambos stages. La corrección pasó en Mesa/OpenGL 3 y en la GPU física.

El modo `kvm-smoke --synthetic` también pasó: H.264 local transmitido mediante dos pares WebRTC reales, decodificado por hardware y presentado a aproximadamente 30 FPS, con cambio a nitidez durante la reproducción. No se conecta a un KVM ni envía HID.

## Entrada y límites de la validación

Las pruebas cubren conservación de distancia, deltas firmados y fraccionales, dirección diagonal, orden de botones/rueda, reintento de transporte, límite de cola, liberación de modificadores y recuperación de indicadores tras errores mientras se está desconectado.

La captura física mediante el portal Wayland y la tasa efectiva de reportes USB requieren una sesión interactiva. No se midió el polling USB ni se comparó la latencia extremo a extremo con el cliente original. Los valores 125/250/500/1000 Hz son objetivos del planificador del cliente.

No se modificó firmware, códec, EDID ni configuración del escritorio. Las ventanas, servidores gráficos y archivos de confianza creados para las pruebas se cerraron o eliminaron al terminar.
