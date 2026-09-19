---
title: Vista general de emit()
description: Un solo entry point maneja tanto handover de metadata de plugin como codegen app-side Kotlin/Swift.
sidebar:
  order: 1
---

`istmo_build::emit()` es la única función que deberías necesitar en un
`build.rs`. Mira el crate en el que corre y hace lo que ese crate
necesita.

## El one-liner

```rust
// build.rs para un plugin — o una app — o ambos
fn main() {
    istmo_build::emit();
}
```

- Lee `istmo.toml` si está presente.
- Emite metadata de plugin (contract + deps nativas) si `[plugin]`
  está seteado.
- Emite env vars app-side wiring si el crate depende de plugins.
- Genera dispatchers Kotlin/Swift si `android/` o `ios/` están al lado
  de `Cargo.toml`.

## Flow de decisión

```
                     ┌─ emit() ────────────────────────────────┐
                     │                                          │
     istmo.toml      │                                          │
     tiene [plugin]? ├── sí ──► emit contract via DEP_*         │
                     │           emit native deps via DEP_*     │
                     │           emit manifest via DEP_*         │
                     │                                          │
     directorio      │                                          │
     android/ o ios/ ├── sí ──► emit_wiring_env (auto-wiring)   │
     al lado de      │           corre emit_app_with(AppOpts)   │
     Cargo.toml?     │           - camina collect_dep_contracts()│
                     │           - auto-detect namespaces/dirs  │
                     │           - escribe .kt/.swift generado  │
                     │                                          │
                     └─ hecho ─────────────────────────────────┘
```

Ambas ramas pueden correr en la misma invocación de `emit()` — un
crate app que también declara su propio plugin es válido.

## Cuándo necesitas más control

Usa `emit_with(AppOpts)` para pasar:

- `extra_contracts: Vec<Contract>` — para crates schema que cargan
  varios contratos, o testbeds con contratos inline que nunca
  publican un crate plugin.
- `per_plugin: HashMap<String, AppPluginOpts>` — override del role
  (`host` vs. `client`) o plataformas por plugin sin tocar
  `istmo.toml`.
- `seed_backends: true` — genera scaffolds one-time
  `<T>BackendImpl.kt/swift` con cuerpos `TODO()` (`write_if_absent`,
  nunca sobrescribe).
- `root`, `android_root`, `ios_root`, `android_package`, `ios_app_dir`
  — override de auto-detección.

Ejemplo:

```rust
// build.rs (en la raíz del crate app)
use istmo_build::{AppOpts, emit_with};
use my_schema_crate::plugin_contract;

fn main() {
    emit_with(AppOpts {
        extra_contracts: vec![
            plugin_contract::permissions(),
            plugin_contract::notifications(),
        ],
        seed_backends: true,
        ..AppOpts::default()
    });
}
```

## Detalles de auto-detección

- **Package de Android**: parseado desde
  `android/app/build.gradle{,.kts}` `namespace = "…"`. Fallback a
  `AndroidManifest.xml package="…"`. Package target default es
  `<namespace>.gen`.
- **App dir de iOS**: escaneado desde `ios/*/`. Si existe exactamente
  un subdir no oculto (excluyendo `*.xcodeproj` y `*.xcworkspace`),
  ese es el app dir. Con ≥2 candidatos, gana el que está pareado con
  un sibling `.xcodeproj`.

Override cualquiera via `[app] android_package = "…"` o
`[app] ios_app_dir = "…"` en el `istmo.toml` de la app.

## Imports runtime

Los dispatchers Kotlin dependen de clases del package
`dev.istmo.runtime` (`IstmoRuntime`, `Bincode`, `PluginException`,
etc.). Cuando el package target no es `dev.istmo.runtime`, `emit()`
prepende un bloque de import automáticamente — nunca escribes esos
imports a mano.

## Saltear una plataforma

Si sólo envías Android o sólo iOS, dile a `emit()` que se saltee la
otra plataforma aunque el directorio exista:

```toml
[app]
ios = false          # nunca generes archivos Swift
```

El loop auto-detect + generador cortocircuita para la plataforma
deshabilitada.

## Testing sin side effects

`emit()` en un crate plugin es idempotente — corridas repetidas de
`cargo build` regeneran los mismos bytes. `write_if_changed` skipea el
filesystem cuando el contenido matchea. Si necesitas inspeccionar qué
sería emitido, imprime manifests en `build.rs`:

```rust
if let Ok(m) = istmo_build::Manifest::from_path("istmo.toml") {
    println!("cargo::warning=parsed manifest: {m:#?}");
}
```

## Auto-registración

Junto con los archivos dispatcher, types y codec por plugin, `emit()`
genera un solo `IstmoPluginRegistry.kt` / `.swift` que cablea cada
dispatcher de plugin elegible en una sola llamada. Ver
[Auto-registración](/es/build-scripts/auto-register/) para la
historia completa.

## Siguiente

- Schema completo del manifest: [Referencia de `istmo.toml`](/es/build-scripts/istmo-toml-reference/).
- Entender el mecanismo DEP_*: [Conceptos → Contratos](/es/concepts/contracts/).
- Auto-wiring `runtime!`: [Avanzado → Auto-wiring](/es/advanced/auto-wiring/).
- Auto-registrando dispatchers: [Auto-registración](/es/build-scripts/auto-register/).
