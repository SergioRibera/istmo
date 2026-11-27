# Captura de pantalla en Apple: lista de verificación en hardware real

Los backends de Apple (`macos-sck` en Rust; `iosSck` / `iosReplayKit` en
Swift) compilan y pasan `clippy -D warnings` y el type-check de Swift
contra stubs, pero **no se han ejecutado nunca en un Mac, iPhone ni
iPad**. Esta lista es lo que hay que comprobar antes de meterlos en
`Auto`. Cada punto dice qué hacer y qué debería verse; si algo no
coincide, anota la salida (`eprintln!` de los tests, `tracing` con
`RUST_LOG=istmo_screen_capture=debug`).

## macOS (Rust, `macos-sck`)

Requisitos: macOS 12.3 o más nuevo (14+ para el picker y
`SCScreenshotManager`, 15+ para HDR), Rust con el target del Mac, el
repositorio en la rama del plugin.

### 1. Permiso de grabación de pantalla (TCC)

```sh
ISTMO_SC_TEST_MACOS=1 cargo test -p istmo-screen-capture --test macos_live grant_caps_and_sources -- --nocapture
```

- La primera vez el test falla a propósito: llama a
  `CGRequestScreenCaptureAccess`, macOS muestra el diálogo (o abre Ajustes
  › Privacidad › Grabación de pantalla). Concede el permiso **a la
  terminal** (o al IDE) y **cierra y vuelve a abrir la terminal**.
- Comprueba que antes de conceder, `access` decía `NotDetermined` y,
  después de pedirlo sin reiniciar, `RequiresRestart`.
- Tras reiniciar: `Granted`, listado `Enumerable`, aparece la pantalla
  principal con `primary: true`, ventanas con título y aplicaciones con
  bundle id. Revisa que `geometry` (puntos) y `physical_size` (píxeles)
  cuadran con la pantalla (p. ej. 1512×982 y 3024×1964 en un MacBook Pro
  14").

### 2. Capturas (`screenshot_pixels`)

```sh
ISTMO_SC_TEST_MACOS=1 cargo test -p istmo-screen-capture --test macos_live screenshot_pixels_of_every_display -- --nocapture
```

- Pantalla principal y `AllMonitors` (con dos pantallas: una imagen
  compuesta, ancho = suma). Guarda una imagen (añade un `std::fs::write`
  temporal) y mírala: colores correctos (RGBA, no BGR), sin franjas, sin
  recorte.
- En macOS 14+ debe usar `SCScreenshotManager`; en 12.3–13 un stream de
  un fotograma. Con `RUST_LOG=debug`, si aparece "the screenshot has no
  IOSurface" anótalo.

### 3. Sesiones: IOSurface, CVPixelBuffer y CPU sin copias

```sh
ISTMO_SC_TEST_MACOS=1 cargo test -p istmo-screen-capture --test macos_live iosurface_frames_without_a_copy_then_cpu_frames -- --nocapture
```

- Tres sesiones (BGRA en `IoSurface`, NV12 en `CvPixelBuffer`, BGRA en
  `Cpu`), diez fotogramas cada una, `copies` vacío, `pts_clock = HostTime`
  y `wall_us` a menos de 1 s del reloj real.
- Comprueba en la salida que `surface_id` se repite (el pool de SCK se
  recicla: 3–8 superficies) y que `buffer.slot` sigue a la superficie.
- Mueve una ventana durante el test: `damage` debe traer rectángulos
  (en píxeles) donde cambió algo; con la pantalla quieta, nada nuevo
  (estado `Idle`).
- Dato a confirmar: `content_rect` = `contentRect` (puntos) ×
  `scaleFactor`. Si en una ventana con letterbox el rectángulo sale el
  doble o la mitad de lo esperado, la unidad es otra: anótalo.

### 4. Agotamiento del pool

```sh
ISTMO_SC_TEST_MACOS=1 cargo test -p istmo-screen-capture --test macos_live a_consumer_holding_every_frame_exhausts_once_then_reads_back -- --nocapture
```

- Con `Stall`: nunca más de `pool_depth - 1` fotogramas retenidos y un
  solo `Exhausted`; la captura no se congela al soltar.
- Con `CopyToCpu`: siguen llegando fotogramas marcados
  `ExhaustionReadback` en memoria CPU.

### 5. Picker (macOS 14+)

```sh
ISTMO_SC_TEST_MACOS_PICKER=1 cargo test -p istmo-screen-capture --test macos_live the_picker_shows_with_allow_picker -- --nocapture
```

- El picker necesita una aplicación con `NSApplication` y run loop
  principal: desde `cargo test` puede **no aparecer** (el hilo principal
  no corre un run loop) y el test acabará con `ConsentRequired` a los
  120 s. Si pasa eso, repítelo desde una app de ejemplo de istmo con
  ventana (abre una sesión con `SourceSelector::Picker` y
  `allow_picker: true`).
- Cancelar el picker → `ConsentDenied`. Elegir una ventana → la sesión
  `source.kind = Window` y el tamaño de esa ventana.

### 6. Audio (macOS 13+)

```sh
ISTMO_SC_TEST_MACOS_AUDIO=1 cargo test -p istmo-screen-capture --test macos_live audio_only_follows_the_wall_clock -- --nocapture
```

- Pon música mientras corre. Llegan trozos S16 estéreo; anota la
  `rate` real (se pide 48 kHz). Si aparece el aviso "delivers audio at
  another rate or channel count", anota qué llega.
- Pide 44 100 Hz (cámbialo en el test): ScreenCaptureKit documenta
  8/16/24/48 kHz; comprueba qué hace con 44,1 kHz (ferricast lo
  necesita para AAC-ELD).
- `ExcludeSelf` no debe capturar el sonido del propio proceso; con
  `Application("com.apple.Music")` sólo esa app.

### 7. Otros puntos sólo verificables en el Mac

- Desenchufar la pantalla capturada → `SourceLost`; detener la captura
  desde el menú del sistema → `ConsentRevoked`.
- `watch_sources`: enchufar/desenchufar pantallas, abrir/cerrar ventanas
  (sondeo cada 1 s).
- HDR (15+, pantalla XDR): `PreferHdr` con `Ar30`; mira si los
  valores superan el blanco SDR.
- `exclude_own_process` y `exclude` (ventanas `window:<id>`, apps
  `app:<bundle>`) desaparecen del fotograma.
- Firma del binario: el permiso TCC va ligado a la firma; re-firmar lo
  pierde (esperado).
- Cuando todo pase: cambiar `joins_auto` a `true` en
  `src/desktop/macos/sck.rs` (y documentarlo en el diseño).

## iOS / iPadOS (Swift)

Requisitos: Xcode con el SDK que corresponda, una app de ejemplo de
istmo con el plugin `istmo.screen_capture` habilitado, y:

- iOS 27 / iPadOS 27 para `iosSck` (ScreenCaptureKit). Para seguir
  capturando en segundo plano: `UIBackgroundModes` con `screen-capture`
  (y `audio` si hace falta).
- Un dispositivo con iOS < 27 para `iosReplayKit`.

Primero el type-check real (en el Mac):

```sh
runtime/ios/ci/typecheck-app.sh <ejemplo>
```

Si falla en `ScreenCaptureCapturers.swift`, compara los nombres con el
SDK de iOS 27 (los stubs siguen el SDK de macOS y el artículo
"Capturing screen content on iOS"): `SCContentSharingPicker.shared`,
`contentSharingPicker(_:didUpdateWith:for:)`,
`SCStreamFrameInfo.videoOrientation` (se asume un `NSNumber` con valor
EXIF 1–8).

Desde Swift (o Rust vía el cliente), con `allowPicker: true` y
`backend: .fixed(.iosSck)` / `.fixed(.iosReplayKit)` (ninguno entra en
`Auto` todavía):

1. `backends()`: en iOS 27 `iosSck` `ready`; en < 27 `osTooOld(27.0.0)`
   e `iosReplayKit` `ready`.
2. `screenshot_pixels`: aparece el picker (27) o la alerta de ReplayKit;
   la imagen es correcta y derecha. En ReplayKit comprueba qué formato
   llega (se espera `420f`): los colores deben ser correctos (conversión
   BT.709).
3. `open` con un sumidero Rust: llegan fotogramas `CvPixelBuffer` /
   `IoSurface` sin copia, `wallUs` correcto. Retén fotogramas en Rust: no
   más de `poolDepth - 1`, un `Exhausted`, y con `CopyToCpu` siguen
   llegando copias.
4. Rotar el dispositivo: `orientation` del fotograma cambia (`videoOrientation`
   en 27, `RPVideoSampleOrientationKey` en ReplayKit) y girar según ella
   deja la imagen derecha.
5. `open_preview` con un `AVSampleBufferDisplayLayer` registrado
   (`IstmoRuntime.shared.registerObject(layer)`): la vista previa se ve,
   `PreviewFit` cambia el encuadre; con `open_preview_with_frames` llegan
   además fotogramas al sumidero.
6. ReplayKit: mandar la app a segundo plano → `paused(backgrounded)`;
   volver → `resumed`.
7. Audio: en 27 `System` / `ExcludeSelf` con contenido que suene; en
   ReplayKit `Application(...)` (sólo el audio de la propia app). Anota
   el formato real (ReplayKit suele dar 16 bits big-endian en
   `.audioApp`: la conversión lo soporta, pero hay que verlo).
8. Detener la captura desde el Centro de control → `consentRevoked` /
   `stopped`.
9. Cuando pase: añadir el backend a `ScreenCaptureBackendImpl.joinsAuto`.
