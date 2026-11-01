# Publicación de macOS

La versión 0.1.0 se compila en Apple Silicon y requiere macOS 26.0: el mínimo
proviene de las bibliotecas del runtime, no solo del ejecutable Rust. No hay
artefactos Intel, Linux ni Windows verificados en esta publicación.

## Compilar y empaquetar

Con las dependencias de desarrollo descritas en README instaladas:

```sh
npm ci
npm run release:macos
```

El script compila Tauri en producción, incorpora GStreamer/libnice y su scanner,
reescribe las dependencias con rutas relativas y verifica arquitectura y versión
mínima de cada biblioteca. Después firma el código ad hoc y genera ZIP, DMG y
`SHA256SUMS` en `.tools/releases/v0.1.0/`. No usa claves de firma privadas.

La aplicación utiliza plugins privados y VideoToolbox para H.264; esta distribución
no incorpora FFmpeg ni un decodificador por software. El directorio
`Contents/Resources/third-party-licenses` contiene avisos, licencias y metadatos de
procedencia. `gstreamer-runtime-manifest.json` enumera los componentes incorporados.

## Verificar e instalar

```sh
shasum -a 256 -c SHA256SUMS
"KVM Client.app/Contents/MacOS/kvm-client" --check-runtime
codesign --verify --deep --strict "KVM Client.app"
```

`--check-runtime` comprueba los elementos multimedia sin abrir ventanas,
preferencias, credenciales, conexiones ni captura HID. La firma ad hoc comprueba
integridad local; no sustituye Developer ID ni notarización. macOS puede bloquear
la aplicación descargada. El DMG permite copiarla a Aplicaciones. No hay
actualizador automático: para actualizar o volver a una versión anterior,
sustituye la aplicación cerrada por el paquete de la versión elegida. Las
preferencias y el llavero se conservan fuera del paquete.

## Publicar

Ejecutar las pruebas, crear un commit y una etiqueta, y adjuntar los artefactos
junto con las fuentes de esa misma etiqueta y sus checksums. Conservar `LICENSE`,
las fuentes vendorizadas de Moonlight y los avisos de SGSR. La licencia del
proyecto es GPL-3.0-or-later. Indicar las pruebas realizadas y los límites de
hardware/plataforma en las notas; no afirmar compatibilidad en equipos no probados.
