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

El template necesita `cargo-generate` 0.21 o mayor.

## Genera un proyecto

```bash
cargo generate --git https://github.com/SergioRibera/istmo-template --name my-app
```

Responde los prompts (arquetipo `app` o `plugin`, bundle id,
plataformas target, plugins iniciales, licencia, CI provider, framework
de UI, versión de istmo) y `cd` al nuevo directorio.

## Qué acaba de pasar

El template envía dos arquetipos — `app` y `plugin` — bajo carpetas
anidadas dentro del repo template. Post-generate el hook **aplasta el
arquetipo elegido a la raíz del proyecto**, así que lo que obtienes es
un layout plano:

```
my-app/
├── Cargo.toml               # el crate de la app
├── build.rs                 # one-liner: istmo_build::emit()
├── istmo.toml               # id, nombre, versión, ícono y overrides de la app
├── justfile                 # recetas build / run / lint
├── flake.nix                # devshell opcional Nix (targets desktop)
├── src/
│   ├── lib.rs               # istmo::runtime!() + entry point #[istmo::mobile_app]
│   ├── app.rs               # estado y UI compartidos de la app
│   ├── main.rs              # entry point desktop
│   └── desktop.rs           # runtime desktop (hosts Rust + emulador)
├── android/                 # proyecto Gradle (invoca cargo)
│   └── app/src/main/AndroidManifest.xml
└── ios/                     # proyecto xcodegen (invoca cargo)
    ├── project.yml
    └── <Name>App/main.swift
```

Sin directorios prefijo `app/` o `plugin/` — cada archivo se sienta en
la raíz del proyecto. Si elegiste el arquetipo `plugin`, el tree es
similar pero con `native/{android,ios}/` como backends de referencia
en lugar de las carpetas de proyecto de plataforma.

- `src/lib.rs` es tu entry point. En Android la activity launcher,
  `dev.istmo.runtime.IstmoGameActivity`, carga la librería y lo corre;
  en iOS `main.swift` le pasa el control con `IstmoApp.run()`.
- `build.rs` corre en cada compilación. Como tienes `android/` e `ios/`
  al lado de `Cargo.toml`, `istmo_build::emit()` además genera los
  shims Kotlin/Swift para cada plugin del que la app depende **y** un
  `IstmoPluginRegistry` para registración nativa one-shot.
- `istmo.toml` declara la identidad de la app y cualquier override
  app-level — ver la
  [referencia `[app]`](/istmo/es/build-scripts/istmo-toml-reference/#app).

## Auto-registración por default

El template no trae ninguna activity Kotlin y su `main.swift` es de una
línea: los entry points del runtime arrancan `IstmoRuntime` y registran
cada plugin mediante el `IstmoPluginRegistry` generado antes de que
corra tu entry point de Rust. En Android el manifest nombra
directamente la activity del runtime:

```xml
<!-- android/app/src/main/AndroidManifest.xml -->
<activity
    android:name="dev.istmo.runtime.IstmoGameActivity"
    android:exported="true">
    <meta-data android:name="android.app.lib_name" android:value="my_app" />
    <!-- intent filter MAIN / LAUNCHER -->
</activity>
```

Haz una subclase de `IstmoGameActivity` y apunta el manifest a tu
clase solo cuando necesites código Kotlin propio.

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
SO de la app viven en `istmo.toml`; el plugin de Gradle `io.github.sergioribera.istmo`
y los settings de Xcode generados los aplican, y ese mismo plugin y la
fase pre-build de Xcode corren cargo por ti. Si falta algo, `cargo
build` imprime avisos `istmo doctor:` y `./gradlew istmoDoctor` revisa
el toolchain. Ver
[Proyectos nativos](/istmo/es/build-scripts/native-projects/).

## Córrelo

### Android

El template no trae el Gradle wrapper; créalo una vez y después
compila e instala:

```bash
cd android
gradle wrapper
./gradlew installDebug
```

El plugin de Gradle `io.github.sergioribera.istmo` invoca a cargo para
producir un `.so` por ABI y lo empaqueta en el APK; Gradle lo instala
en el dispositivo o emulador conectado. `just build-android` compila
el APK debug.

### iOS

Corre primero un `cargo build` (o `cargo check`): escribe
`ios/<Name>App/istmo-plugins.yml` (fuentes de los plugins, identidad de
la app, settings de link, la fase pre-build de cargo), que
`project.yml` incluye, e `ios/.istmo/`.

```bash
cargo check
cd ios
xcodegen generate
open <Name>App.xcodeproj
```

La fase pre-build de Xcode invoca cargo para producir un
`libmy_app.a`, que se linkea dentro de la app Swift. `just build-ios`
hace lo mismo para el simulador.

### Desktop

```bash
just run    # cargo run
```

`src/desktop.rs` arranca un runtime in-process: los plugins con backend
desktop corren como hosts Rust, las llamadas al resto fallan con una
línea de log hasta que las emules.

## Adónde ir después

- Añadir un plugin: [Escribiendo tu primer plugin](/istmo/es/getting-started/first-plugin/).
- Entender las piezas: [Arquitectura](/istmo/es/concepts/architecture/).
- Enviar un servicio de background: [Services y workers](/istmo/es/writing-plugins/services-workers/).
