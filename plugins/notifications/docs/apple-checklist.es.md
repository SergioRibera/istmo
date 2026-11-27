# istmo-notifications: lista de verificación en Mac y dispositivo

Nada de esto se ejecutó: el backend de iOS solo pasó el type-check con
stubs (`scripts/check-plugin-native.sh`) y sus partes de Foundation pura
(regla de calendario contra los vectores, tiempos, entregas perdidas,
plan de armado, validación) corren en Linux con
`tests/native/ios/check.sh`; el de macOS solo `cargo clippy` cruzado (sin
SDK, sin enlazar). Cada punto dice qué mirar y qué anotar.

## Preparación

- macOS: empaquetar la app de ejemplo en un `.app` con
  `CFBundleIdentifier`, firmarla (ad hoc vale) y lanzarla desde Finder o
  `open Demo.app`. Llamar a `DesktopNotifications::attach(&runtime)` antes
  del bucle de eventos.
- iOS: app generada por la plantilla, con el plugin en `istmo::runtime!`;
  comprobar que `IstmoPluginRegistry.swift` registra
  `NotificationsDispatcher(backend: NotificationsBackendImpl(), ...)`.
- Para comunicación: `[app.notifications] apple = { communication = true }`
  (entitlement `com.apple.developer.usernotifications.communication` y
  `INSendMessageIntent` en `NSUserActivityTypes`).

## macOS

1. Binario suelto (`cargo run`): toda llamada devuelve `NotBundled`, un
   solo `warn` en el log, `capabilities()` sin nada nativo, ningún crash
   dentro de `UNUserNotificationCenter`.
2. `.app`: `request_permission` muestra el diálogo; `permission()`
   refleja lo elegido (y `time_sensitive` / `communication` en 12+).
3. `show` con título, subtítulo, cuerpo, imagen (`Png`): se ve la
   miniatura; el archivo de caché del host sigue existiendo después
   (el adjunto es un enlace duro o una copia en `$TMPDIR/istmo-notifications`).
4. Acciones: 1, 4 y 12 acciones (12 > 10: aparece "More" y la respuesta
   trae `overflow_actions`). Respuesta inline (`Reply`): el texto llega en
   `reply`. Elecciones (`Select`): un botón por opción, `reply` = la opción.
5. Descartar la notificación desde el Centro de Notificaciones: llega
   `NotificationEvent::Dismissed(User)` (acción de descarte propia de la
   categoría).
6. App en primer plano: con `hide_in_foreground` no aparece banner; con
   `AppleExtras.foreground` se respetan banner / lista / sonido.
7. Progreso con `LiveNotification` a 10 Hz: como mucho 2 publicaciones por
   segundo, sin sonido ni banner repetido (pasiva, solo lista); anotar si
   macOS vuelve a mostrar banner al reemplazar.
8. Toque con la app cerrada: el sistema relanza la app; la respuesta llega
   con `launched_app = true`. Anotar si `NSApplicationLaunchUserNotificationKey`
   viene en el `userInfo` de `didFinishLaunching` (si no, `launched_app`
   queda en false). Un segundo toque, o uno más de 10 s después del
   arranque, trae `false`; abrir la app con un documento y tocar una
   notificación pasados 10 s también.
8b. `attach` después de `NSApplication::run`: un `warn` en el log al
   arrancar el backend.
8c. App con delegado propio puesto antes de `attach`: sus notificaciones
   le siguen llegando (`willPresent` / `didReceive` reenviados); las del
   plugin no. Con el delegado puesto después: llamar a
   `forward_will_present` / `forward_did_receive` y comprobar que las del
   plugin responden.
8d. macOS 10.15 (si hay una máquina): primer plano con banner = `.alert`;
   una actualización silenciosa no muestra nada en primer plano.
9. Comunicación (12+): conversación con remitente y avatar: se ve el
   avatar del remitente; sin el entitlement se ve como líneas de texto y
   el log dice por qué (`interaction not donated` / `content refused`).
10. Niveles (12+): `Urgent` = sensible al tiempo solo con
    `IstmoNotifications.TimeSensitive` en el `Info.plist` del `.app` (si
    no, activo); `Critical` = crítico solo con `Critical` (si no, un
    escalón menos) y su sonido crítico igual; `Passive` sin sonido.
11. Categorías: la app con categorías propias no las pierde, tampoco si
    las registra después de la primera notificación del plugin (a los 2 s
    el plugin registra la unión de nuevo). Con más de 128 categorías del
    plugin, las que usan notificaciones entregadas o pendientes siguen.
11b. Permiso denegado en Ajustes: `show` devuelve `PermissionDenied`.
12. `open_settings()` abre Ajustes del Sistema > Notificaciones > la app.
13. `delivered()` lista las del plugin con grupo (`threadIdentifier`) y
    fecha.

## iOS / iPadOS

1. Arranque en frío por toque: la respuesta llega a Rust (cola
   `istmo.notifications.responses`) con `launched_app = true` (primera
   respuesta del proceso, dentro de 10 s desde `install()`). Una segunda
   acción en segundo plano en el mismo proceso trae `false`.
2. Acción en segundo plano (`opens_app = false`): la app no pasa a primer
   plano, la respuesta trae `ack`; `complete(ack)` libera el completion
   handler; sin `complete`, se libera a los 25 s.
3. Posponer (`Snooze`): se oculta y vuelve tras el retardo aunque la app
   esté cerrada (solicitud `<id>#s`); evento `Snoozed`. Posponer una
   repetición no la reemplaza: la siguiente ocurrencia sigue armada.
4. Programación: `After`, `At`, `Every` (>= 60 s; menos = `Invalid
   trigger.every`), `Calendar` diario, semanal con 3 días, con zona
   horaria (una zona desconocida = `Invalid trigger.time_zone`). Cada
   ocurrencia es una solicitud de una vez (`<id>` o `<id>@<ms>`);
   `pending()` muestra `next_fire_ms` correcto. Regla a las 02:30 en el
   día del cambio de hora de Nueva York: dispara a las 03:30.
5. Más de 64: programar 100; quedan 60 ocurrencias armadas
   (`getPendingNotificationRequests`), el resto en
   `Application Support/istmo-notifications/schedule.bin`; tras
   dispararse algunas, al volver a primer plano se reponen. Las que no
   tuvieron hueco con la app cerrada siguen su `MissedPolicy` al volver
   (`DeliveredLate` dentro de la ventana, nada con `Drop`).
6. `until_ms`: la última ocurrencia armada es la última dentro del
   límite (sin disparos de más).
6b. Cancelar mientras se repone: nada de lo cancelado queda armado.
6c. `cancel_all()`: las notificaciones propias de la app (sin `istmo.id`)
   siguen.
7. Actualización silenciosa (`set_progress`): sin sonido, nivel pasivo;
   anotar si el banner reaparece (punto abierto del diseño).
8. Adjuntos: `Png`, `Jpeg`, `File` (también un archivo sin extensión:
   el tipo sale de los primeros bytes); `Named` no adjunta nada.
9. Bloqueo (`LockScreen::HideContent`): con previsualizaciones ocultas se
   ve el texto genérico.
10. Comunicación (iOS 15): avatar del remitente en la notificación; el
    foro reporta avatares ausentes en notificaciones locales: anotarlo.
11. Interrupción crítica sin entitlement: se degrada a sensible al tiempo.
11b. Permiso denegado: `show` y las actualizaciones devuelven
    `PermissionDenied`.
11c. Actualizaciones silenciosas de una conversación y ocurrencias
    programadas: no se dona otra `INInteraction` (comprobar que la
    notificación sigue con aspecto de comunicación sin la donación).
12. Canales estáticos: `istmo_notification_channels.bin` en el bundle
    (generado por istmo-build) se lee al instalar.
13. El badge de la app no cambia nunca.
