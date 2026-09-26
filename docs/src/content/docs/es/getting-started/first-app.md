---
title: Tu primera app
description: Bootstrap de una app móvil 100% Rust que corre en Android e iOS con un solo comando.
sidebar:
  order: 2
---

El camino más rápido a una app que funcione es usar
[`cargo-generate`](https://github.com/cargo-generate/cargo-generate)
contra el template oficial. El template genera un crate Rust, un
proyecto Gradle de Android y un proyecto Xcode de iOS ya cableados
entre sí.

## Instala cargo-generate

```bash
cargo install cargo-generate
```

## Genera un proyecto

```bash
cargo generate --git https://github.com/sergioribera/istmo-template --name my-app
```

Responde los prompts (arquetipo `app` o `plugin`, bundle id,
plataformas target, plugins iniciales, licencia, CI provider, framework
de UI) y `cd` al nuevo directorio.

## Qué acaba de pasar

El template envía dos arquetipos — `app` y `plugin` — bajo carpetas
anidadas dentro del repo template. Post-generate el hook **aplasta el
arquetipo elegido a la raíz del proyecto**, así que lo que obtenés es
un layout plano:

```
my-app/
├── Cargo.toml               # el crate de la app
├── build.rs                 # one-liner: istmo_build::emit()
├── istmo.toml               # config app-side del codegen
├── flake.nix                # devshell opcional Nix (targets desktop)
├── src/
│   ├── lib.rs               # entry point #[istmo::mobile_app]
│   ├── app.rs               # código de UI compartido
│   └── main.rs              # entry point desktop
├── android/                 # proyecto Gradle (invoca cargo)
│   └── app/src/main/kotlin/<pkg>/<Name>Activity.kt
└── ios/                     # proyecto Xcode (invoca cargo)
    └── <Name>App/main.swift
```

Sin directorios prefijo `app/` o `plugin/` — cada archivo se sienta en
la raíz del proyecto. Si elegiste el arquetipo `plugin`, el tree es
similar pero con `native/{android,ios}/` como backends de referencia
en lugar de las carpetas de proyecto de plataforma.

- `src/lib.rs` es tu entry point. En Android lo invoca `NativeActivity`;
  en iOS un `App` struct de Swift.
- `build.rs` corre en cada compilación. Como tenés `android/` e `ios/`
  al lado de `Cargo.toml`, `istmo_build::emit()` además genera los
  shims Kotlin/Swift para cada plugin del que la app depende **y** un
  `IstmoPluginRegistry` para registración nativa one-shot.
- `istmo.toml` declara cualquier override app-level — ver la
  [referencia `[app]`](/istmo/es/build-scripts/istmo-toml-reference/#app).

## Auto-registración por default

La activity de Android y el `main.swift` de iOS son de una línea: los
entry points del runtime arrancan `IstmoRuntime` y registran cada plugin
mediante el `IstmoPluginRegistry` generado antes de que corra tu entry
point de Rust:

```kotlin
// android/app/src/main/kotlin/<pkg>/<Name>Activity.kt
class MainActivity : IstmoGameActivity()
```

```swift
// ios/<Name>App/main.swift
import IstmoRuntime

IstmoApp.run()
```

Cada plugin cuyo `istmo.toml` declara `auto_register = true` (el
default) queda registrado, incluidos los plugins cuyo backend Android
necesita la activity host. Ver
[Auto-registración](/istmo/es/build-scripts/auto-register/) para el
mecanismo.

## Una sola fuente de verdad

El id, nombre, versión, número de build, ícono y versiones mínimas de
SO de la app viven en `istmo.toml`; el plugin de Gradle `dev.istmo.app`
y los settings de Xcode generados los aplican, y ese mismo plugin y la
fase pre-build de Xcode corren cargo por vos. Si falta algo, `cargo
build` imprime avisos `istmo doctor:` y `./gradlew istmoDoctor` revisa
el toolchain. Ver
[Proyectos nativos](/istmo/es/build-scripts/native-projects/).

## Córrelo

### Android

```bash
cd android
./gradlew installDebug
```

Gradle invoca a cargo (via un build task) para producir un `.so` por
ABI, lo copia dentro del APK, y lo instala al dispositivo o emulador
conectado.

### iOS

```bash
cd ios
xcodegen
open <Name>App.xcodeproj
```

La build phase de Xcode invoca cargo para producir un `libmy_app.a`,
que se linkea dentro de la app Swift.

## Adónde ir después

- Añadir un plugin: [Escribiendo tu primer plugin](/istmo/es/getting-started/first-plugin/).
- Entender las piezas: [Arquitectura](/istmo/es/concepts/architecture/).
- Enviar un servicio de background: [Services y workers](/istmo/es/writing-plugins/services-workers/).
