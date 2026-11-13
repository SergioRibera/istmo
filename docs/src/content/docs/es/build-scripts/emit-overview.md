---
title: Visión general de emit()
description: Un solo entry point maneja tanto el traspaso de metadatos del plugin como el codegen Kotlin/Swift del lado de la app.
sidebar:
  order: 1
---

`istmo_build::emit()` es la única función que deberías necesitar en un
`build.rs`. Mira el crate en el que corre y hace lo que ese crate
necesita.

## La línea única

```rust
// build.rs de un plugin — o de una app — o de ambos
fn main() {
    istmo_build::emit();
}
```

`istmo-build` va en `[build-dependencies]`. En una sola ejecución,
`emit()`:

1. Lee y valida `istmo.toml` (si falta, la configuración está vacía).
2. Si el archivo declara entradas `[plugin]` / `[[plugin]]`, entrega los
   metadatos del plugin a los crates dependientes.
3. Compara el `[min_versions]` de cada plugin con el piso de la app para
   el target que se compila.
4. Ejecuta la pasada del lado de la app. Siempre empieza grabando la
   lista de plugins auto-conectados en `ISTMO_AUTO_PLUGINS` /
   `ISTMO_AUTO_REMOTE` para `istmo::runtime!` (también en crates de
   plugin, y también cuando un build script llama directamente a
   `emit_app_with`).
5. Cuando el crate es una app, la misma pasada sigue: codegen
   Kotlin/Swift y sincronización de los proyectos nativos cuando hay
   `android/` o `ios/` junto a `Cargo.toml`, extras de escritorio cuando
   el crate compila un binario. Los permisos, las entradas de
   `Info.plist` y los shims de servicio salen de los plugins
   **conectados**: los clients auto-conectados más cada client que
   nombran las secciones `plugins:` / `remote:` de `istmo::runtime!` en
   `src/lib.rs` o `src/main.rs` (un trait en `remote:` cuenta como su
   `<Trait>Client`).

## Flujo de decisión

```
emit()
 │
 ├─ ¿istmo.toml tiene [plugin]? ── sí ─► cargo:ISTMO_MANIFEST, cargo:PLUGIN_IDS
 │                                       cargo:ISTMO_CONTRACTS   (entradas con client_type)
 │                                       cargo:ISTMO_NATIVE_DEPS (si hay [[gradle]] / [[swift_package]])
 │                                       cargo:ISTMO_ASSETS      (si hay [[assets]])
 │                                       cargo:ISTMO_NATIVE_<OS> (por cada dir native/<os>/)
 │
 ├─ siempre ─► comprobación de [min_versions] para el SO del target
 │             ISTMO_AUTO_PLUGINS / ISTMO_AUTO_REMOTE (rustc-env)
 │
 ├─ ¿android/, ios/ o un target binario? ── no ─► listo (crate de plugin puro)
 │
 ├─ sí ─► ISTMO_APP_BUNDLE_ID, ISTMO_APP_ASSETS_DIR, ISTMO_SINGLE_INSTANCE
 │        ícono de ventana + recursos de Windows (targets de escritorio)
 │        prepara [[assets]] para cada plataforma activa
 │
 └─ ¿android/ o ios/ activos? ── sí ─► recorre collect_dep_contracts()
                                       escribe <T>Dispatcher / Types / CodecsImpl (.kt / .swift)
                                       escribe IstmoPluginRegistry (+ IstmoMain.swift)
                                       sincroniza ios/.istmo/, y android/.istmo/ en targets Android
                                       permisos, servicios, entradas de plist
                                         (auto-conectados + runtime! plugins: / remote:)
                                       warnings del doctor
```

Las dos mitades pueden correr en la misma invocación — un crate de app
que además declara su propio plugin es válido.

## Cuando necesitas más control

Usa `emit_with(AppOpts)` para pasar:

- `extra_contracts: Vec<Contract>` — para contratos que ninguna
  dependencia entrega: los construidos por un crate de schema en tiempo
  de build o inline en `build.rs`. Un crate de plugin con varias
  entradas `[[plugin]]` ya los entrega todos. Un contrato extra con el
  id de plugin de uno entregado lo reemplaza (con un warning).
- `extra_manifests` / `extra_native_ios_dirs` — manifests de plugin y
  directorios `native/ios/` a tratar como si los hubiera entregado una
  dependencia. Los plugins de `extra_manifests` se auto-conectan igual
  que los entregados, así que su `client_type` debe resolver en la app.
- `per_plugin: HashMap<String, AppPluginOpts>` — sobrescribe el rol
  (`host` vs. `client`), las plataformas o `auto_register` por id de
  plugin o nombre de tipo, encima de `[[app.plugin]]`.
- `seed_backends: true` — genera esqueletos `<T>BackendImpl.kt/swift`
  de una sola vez con cuerpos `TODO()` / `fatalError` (sólo se escriben
  si no existen, nunca se sobrescriben). Se omite en plugins que traen
  su propio backend.
- `auto_register: Some(false)` — deja vacíos los registries de plugins
  (se siguen escribiendo, sin entradas).
- `lib_name` — el nombre de la librería nativa que usan los shims de
  servicio generados (default: el nombre de lib del crate).
- `root`, `android`, `android_root`, `android_package`, `ios`,
  `ios_root`, `ios_app_dir`, `ios_plugins_subdir` — sobrescriben la
  auto-detección y las claves `[app]` correspondientes.

Ejemplo, de `examples/rust-mobile-demo/build.rs`:

```rust
// build.rs (en la raíz del crate de la app)
use istmo_build::AppOpts;
use istmo_plugins_schema as plugin_contract;

fn main() {
    istmo_build::emit_with(AppOpts {
        extra_contracts: vec![
            plugin_contract::permissions(),
            plugin_contract::notifications(),
            plugin_contract::admob(),
        ],
        ..AppOpts::default()
    });
}
```

`emit_from(manifest_path, source_path)` es sólo la mitad del lado del
plugin, para un crate de plugin cuyo `istmo.toml` o fuente del trait no
están en la ubicación por defecto; `emit_manifest_metadata_with_contracts(path,
&contracts)` entrega contratos construidos a mano en lugar de extraídos.

## Detalles de la auto-detección

- **Paquete Android**: se lee de `namespace = "…"` en
  `android/app/build.gradle{,.kts}`. Si no, de `package="…"` en
  `AndroidManifest.xml`. El paquete destino por defecto es
  `<namespace>.gen`.
- **Directorio de la app iOS**: se busca en `ios/*/`. Si existe
  exactamente un subdirectorio no oculto (excluyendo `*.xcodeproj` y
  `*.xcworkspace`), ese es el directorio de la app. Con ≥2 candidatos,
  gana el que tiene un `.xcodeproj` hermano.

Sobrescribe cualquiera con `[app] android_package = "…"` o
`[app] ios_app_dir = "…"` en el `istmo.toml` de la app. Si ninguno se
resuelve, el build avisa y omite las fuentes de esa plataforma.

Un plugin que trae su propio backend Kotlin declara
`[plugin] android_package` en su `istmo.toml`; su dispatcher, tipos y
codecs se generan en ese paquete en lugar del de la app, así que las
fuentes `native/android/` del plugin compilan con cualquier paquete de
app. Sólo los contratos sin él (los plugins propios de la app, los
contratos built-in) siguen `[app] android_package`.

## Archivos generados

Para cada contrato y plataforma activa (rol `host` salvo override):

| Plataforma | Ubicación | Archivos |
| --- | --- | --- |
| Android | `android/app/src/main/java/<paquete>/` | `<T>Dispatcher.kt`, `<T>Types.kt`, `<T>CodecsImpl.kt` (rol `client`: `<T>Codecs.kt` + `<T>Client.kt` en lugar del dispatcher) |
| iOS | `ios/<AppDir>/<PluginsSubdir>/<T>/Generated/` | `<T>Dispatcher.swift`, `<T>Types.swift`, `<T>CodecsImpl.swift` (rol `client`: `<T>Client.swift` en lugar del dispatcher) |

Los archivos Kotlin importan sólo los tipos de `dev.istmo.runtime` que
usan, así que compilan en cualquier paquete.

## Omitir una plataforma

Si sólo publicas para Android o sólo para iOS, dile a `emit()` que omita
la otra plataforma aunque exista el directorio:

```toml
[app]
ios = false          # nunca generar archivos Swift
```

## Probar sin efectos secundarios

Los archivos generados sólo se escriben cuando cambia su contenido, y
cada entrada que lee la pasada se registra con `cargo:rerun-if-changed`,
así que recompilar con las mismas entradas no toca nada. Si necesitas
inspeccionar lo que se emitiría, imprime los manifests en `build.rs`:

```rust
if let Ok(m) = istmo_build::Manifest::from_path("istmo.toml") {
    println!("cargo::warning=parsed manifest: {m:#?}");
}
```

## Auto-registro

Junto a los archivos de dispatcher, tipos y codecs por plugin, `emit()`
genera un único `IstmoPluginRegistry.kt` / `.swift` que conecta el
dispatcher de cada plugin elegible en una sola llamada. Ver
[Auto-registro](/istmo/es/build-scripts/auto-register/) para la historia
completa.

## Siguiente

- Schema completo del manifest: [Referencia de `istmo.toml`](/istmo/es/build-scripts/istmo-toml-reference/).
- Entender el mecanismo DEP_*: [Conceptos → Contratos](/istmo/es/concepts/contracts/).
- Auto-wiring de `runtime!`: [Avanzado → Auto-wiring](/istmo/es/advanced/auto-wiring/).
- Auto-registro de dispatchers: [Auto-registro](/istmo/es/build-scripts/auto-register/).
