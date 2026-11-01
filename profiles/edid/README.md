# PicoKVM: monitor HDMI 720p

`kvm-720p60.bin` es un EDID HDMI 1.3 de 256 bytes cuyo modo preferido y de mayor
resolución anunciado es **1280 × 720 progresivo a 60 Hz**. Incluye además
**640 × 480 a 59,94 Hz** como modo de recuperación requerido por CTA.
`kvm-720p60.hex` contiene los mismos bytes en
hexadecimal, para editores que utilicen ese formato. Nombre del monitor:
`KVM 720p60`. No anuncia audio HDMI ni resoluciones superiores a 720p.

Este archivo **no limita el codificador del KVM a 1 FPS**. El refresco HDMI del
monitor y los FPS de captura/codificación son controles distintos. No se ofrece
un modo HDMI de 1 Hz como sustituto de un límite de captura no verificado.

## Preparación y recuperación

1. Exporta el EDID actual del KVM y conserva una copia antes de reemplazarlo.
2. Importa el archivo binario, o copia el contenido hexadecimal sin espacios ni
   salto de línea en el editor EDID compatible. No cambies el firmware.
3. Comprueba que el equipo remoto detecta `KVM 720p60` y permite seleccionar
   1280 × 720. El sistema operativo puede conservar ajustes de pantalla previos.
4. Si necesitas volver a la configuración anterior, restaura la copia original.

Cambiar este EDID afecta a la salida HDMI conectada al KVM; no crea por sí solo
una pantalla virtual independiente para Sunshine. No se ha aplicado este perfil
a ningún dispositivo desde el generador.

## Generación y verificación

```sh
python3 scripts/generate-edid-720p.py
```

El generador comprueba encabezado, versión, bloques, checksums, temporización
preferida, fallback VGA y bloque de capacidades de video (VCDB). El DTD utiliza
74,25 MHz y totales de 1650 × 750; la extensión CTA anuncia VIC 4 como nativo y
VIC 1 como fallback. El VCDB declara subescaneo y rango RGB seleccionable.
EDID 1.3 evita la incompatibilidad que el comprobador oficial detecta entre
EDID 1.4 y el bloque de identificación HDMI. Los archivos son perfiles de
configuración, no actualizaciones de firmware.

La estructura se revisó también contra el código de
[edid-decode, bloque CTA](https://github.com/gjasny/v4l-utils/blob/master/utils/edid-decode/parse-cta-block.cpp)
y [bloque base](https://github.com/gjasny/v4l-utils/blob/master/utils/edid-decode/parse-base-block.cpp).
No se ejecutó el comprobador completo `edid-decode`; las verificaciones del
generador y la revisión de campos no equivalen a una aprobación de conformidad
completa ni prueban compatibilidad con el receptor HDMI del dispositivo.

La API oficial de PicoKVM dispone de `getEDID` y `setEDID`; el límite de FPS del
codificador debe comprobarse aparte en la versión instalada:
[registro RPC oficial](https://github.com/LuckfoxTECH/kvm/blob/luckfox/jsonrpc.go).

El perfil y su temporización se han verificado como archivo. No se ha probado
la importación con el firmware del dispositivo, ni se ha confirmado el límite
de longitud del componente nativo que recibe `setEDID`. Conserva el original y
verifica la lectura posterior antes de considerar aplicado el cambio.
