# Captura y programación de entrada

El mouse y el teclado se capturan en Rust. Ningún evento individual atraviesa
React ni depende de `requestAnimationFrame`.

## Uso en Wayland

1. Conecta el KVM y activa **Capturar**.
2. Autoriza la sesión del portal si el escritorio muestra una solicitud.
3. Lleva el cursor al borde derecho exterior de una pantalla para activar el
   control. Este paso responde al mecanismo de activación estándar de InputCapture.
4. Pulsa **Ctrl+Alt+Esc** para cerrar la sesión de captura y recuperar el cursor.

Se usa `org.freedesktop.portal.InputCapture` mediante ashpd y el receptor nativo de
libei. Las notificaciones distinguen preparación, espera en el borde, captura
activa y errores. Se exige permiso para teclado y mouse: sin teclado no se puede
garantizar el atajo local de liberación. No se necesita leer `/dev/input`, cambiar
grupos ni ejecutar el cliente como administrador.

La sesión se cierra ante desactivación solicitada, atajo de emergencia o error.
Si el compositor desactiva temporalmente la captura, se envían reportes de
liberación antes de volver a esperar al borde. La aplicación debe llamar
`capture(false)` al desconectar y cuando pierda foco con captura ya activa; no
debe cancelar la solicitud sólo porque el diálogo del portal tome el foco.

## X11 y XWayland

En una sesión X11 se usa XInput2, con deltas crudos de los dispositivos maestros
para evitar duplicar eventos. El mouse se confina a la ventana enfocada y se
captura el teclado. El mismo atajo libera ambos. Cerrar la conexión X11 también
libera los dispositivos.

Para usar explícitamente este backend en una sesión Wayland, inicia la aplicación
con `GDK_BACKEND=x11 KVM_INPUT_BACKEND=x11`. No se cambia automáticamente desde
Wayland a XWayland después de un rechazo del portal: eso podría capturar una
ventana distinta de la consola.

## Deltas, reportes y congestión

- Frecuencias: 125, 250, 500 y 1000 Hz. La predeterminada es 125 Hz.
- Sensibilidad: 0.1–4.0, valor inicial 1.0. Las fracciones, incluidas las negativas,
  se conservan entre eventos y ticks.
- Un reloj monotónico marca el envío; los retrasos no se recuperan mediante una
  ráfaga de ticks. Se emite como máximo un reporte HID por tick.
- Los movimientos adyacentes con iguales botones se combinan. Cada reporte limita
  sus deltas a −127…127; el resto queda pendiente. La distancia total se conserva
  hasta transmitirla o hasta una liberación explícita de la captura.
- Los cambios de botones, rueda y teclado forman barreras de orden. Un clic no
  adelanta un movimiento pendiente y un movimiento no cruza un cambio de botones.
- El envío es una notificación JSON-RPC, sin respuesta individual. El elemento
  pendiente sólo se confirma cuando el transporte acepta el mensaje.
- Después de errores de envío se reduce la frecuencia de intento, hasta 125 Hz.
  Tras 32 envíos aceptados se recupera gradualmente la frecuencia solicitada.
- La cola admite hasta 1024 elementos y hasta un millón de unidades por segmento
  de movimiento. Superar el límite devuelve un error y libera la captura. Un
  bloqueo de transporte de más de 250 ms durante captura también la libera.
- Una liberación descarta movimientos obsoletos y prioriza mouse sin botones y
  teclado sin teclas. Si el transporte ya está desconectado, esos reportes no
  pueden garantizarse en el host; se mantienen para reintento mientras viva el motor.

El contador de Hz mide reportes aceptados por el transporte por segundo, incluidos
los reportes de teclado y rueda. Con el mouse quieto puede ser cero. No mide el
polling USB ni garantiza la misma tasa en el equipo remoto.

Los códigos físicos evdev se traducen a usos USB HID, con modificadores izquierdo
y derecho separados. El teclado sigue el límite de seis teclas simultáneas de un
teclado USB boot; el exceso usa `ErrorRollOver`. El host remoto aplica su propia
distribución de teclado.

## Pruebas

Las pruebas unitarias del planificador verifican división de movimientos grandes,
fracciones negativas, orden de arrastre/clic/rueda, reintento sin consumo de deltas,
límite de cola y prioridad de la liberación. Las pruebas físicas de captura y la
tasa USB requieren una sesión gráfica y un KVM conectados.
