---
title: Proyectos nativos
description: Cómo build.rs, istmo.toml y la integración con Gradle / Xcode mantienen android/ e ios/ sincronizados, sin CLI.
sidebar:
  order: 4
---

istmo no tiene CLI. Una app es un crate de Cargo con un proyecto Gradle
en `android/` y/o un proyecto Xcode en `ios/` al lado, y tres piezas los
mantienen sincronizados:

| Pieza | Corre | Hace |
| --- | --- | --- |
| `build.rs` → `istmo_build::emit()` | en cada `cargo build` / `cargo check` | codegen, metadata de plugins, identidad de la app, avisos del doctor |
| Plugin de Gradle `io.github.sergioribera.istmo` | en cada build de Gradle | compila el crate Rust, linkea plugins, aplica `[app]` |
| `istmo-plugins.yml` + `ios/.istmo/build-rust.sh` | en cada build de Xcode | compila el crate Rust, linkea plugins, aplica `[app]` |

`istmo.toml` es la única fuente de verdad. Los manifests nativos
(`AndroidManifest.xml`, `Info.plist`) siguen siendo tuyos; referencian
valores que provee istmo.

## Identidad de la app: `[app]`

```toml
[app]
id      = "com.example.myapp"   # applicationId + PRODUCT_BUNDLE_IDENTIFIER
name    = "Mi App"              # label del launcher / CFBundleDisplayName
build   = 12                    # versionCode / CFBundleVersion (default 1)
# version = "1.2.0"             # default: `package.version` de Cargo.toml

[app.icon]
image   = "assets/icon.png"     # ícono de iOS + mipmaps legacy de Android
desktop = "assets/icon-tile.png" # ventana + ícono del .exe de Windows (default: image)

[app.icon.android]              # ícono adaptativo, Android 8+ (API 26)
foreground = "assets/icon-foreground.xml"  # vector drawable .xml, o un .png de 432 px
background = "#EFE6D4"                     # #RRGGBB / #AARRGGBB, .xml o .png
monochrome = "assets/icon-monochrome.xml"  # opcional: íconos temáticos (Android 13+)

[min_versions]
android = 24                    # minSdk
ios     = "15.0"                # IPHONEOS_DEPLOYMENT_TARGET
```

Todas las claves son opcionales. Omite `id` cuando Android e iOS
necesitan identificadores distintos: los proyectos nativos conservan los
suyos.

### Ícono de launcher: `[app.icon]`

Todas las claves son opcionales; pon las que necesiten tus plataformas.

- `image` — una imagen cuadrada (se recomienda 1024 px). Sale de ahí el
  app icon set de iOS (aplanado sobre blanco: la App Store rechaza alfa)
  y los `mipmap-*dpi/istmo_launcher.png` legacy de Android.
- `[app.icon.android]` — las capas de un
  [ícono adaptativo](https://developer.android.com/develop/ui/views/launch/icon_design_adaptive).
  En esta tabla `foreground` y `background` son obligatorias y
  `monochrome` es opcional. Una capa es un vector drawable de Android
  (`.xml`, se copia tal cual a `drawable/`) o un PNG (se reescala a
  108 dp en cada densidad, 432 px en xxxhdpi). El fondo también puede
  ser un color. Deja el dibujo dentro de la zona segura central de
  66 dp: los launchers recortan el borde.

El ícono adaptativo se escribe como `mipmap-anydpi-v26/istmo_launcher.xml`,
con el mismo nombre de recurso que los PNG legacy, así que `${istmoIcon}`
sigue siendo `@mipmap/istmo_launcher` y Android elige el adaptativo en
API 26+. Sin `image`, los dispositivos anteriores a Android 8 quedan sin
ícono (el build avisa si `[min_versions] android` es menor a 26).

## Escritorio

Al compilar para escritorio, `istmo_build::emit()` usa
`[app.icon] desktop` (o `image` si no está): en escritorio nadie recorta
el ícono, así que `desktop` tiene que apuntar a una imagen ya con su
forma.

- **Ícono de la ventana.** Se deja un PNG de 256 px en `OUT_DIR`;
  `istmo::app_icon!()` se expande a `Option<&'static [u8]>` con sus bytes
  (`None` en mobile o sin ícono). Pásaselo a tu toolkit de UI, por
  ejemplo Freya:

  ```rust
  let mut window = WindowConfig::new(app);
  if let Some(png) = istmo::app_icon!() {
      window = window.with_icon(LaunchConfig::window_icon(png));
  }
  ```

  Los compositores Wayland ignoran el ícono de la ventana y usan la
  entrada `.desktop` que coincide con el app id.
- **Ejecutable de Windows.** Se compila un `.ico` con varias resoluciones
  (16–256 px) dentro del `.exe` mediante un `.rc` generado
  ([`embed-resource`](https://crates.io/crates/embed-resource): `rc.exe`
  en MSVC, `windres` / `llvm-rc` al compilar cruzado). El mismo paso
  embebe el manifiesto de aplicación armado con todos los
  `[[windows_manifest_fragment]]` (ver la referencia de `istmo.toml`).

## Android

El plugin está publicado en Maven Central; declara su versión una sola
vez en `android/settings.gradle.kts`, junto a los repositorios de los
que se resuelve:

```kotlin
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
    plugins {
        id("io.github.sergioribera.istmo") version "0.1.1-alpha.5" // la misma que los crates de istmo
    }
}
```

Después aplícalo al módulo de la app:

```kotlin
// android/app/build.gradle.kts
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("io.github.sergioribera.istmo")
}

android {
    namespace = "com.example.myapp"
    compileSdk = 34
    defaultConfig {
        targetSdk = 34
        ndk { abiFilters += setOf("arm64-v8a", "x86_64") }
    }
}
```

y referencia los placeholders desde el manifest:

```xml
<application
    android:label="${istmoLabel}"
    android:icon="${istmoIcon}">
    <activity android:name="dev.istmo.runtime.IstmoGameActivity" android:exported="true">
        <meta-data android:name="android.app.lib_name" android:value="my_app" />
        <!-- intent filter MAIN / LAUNCHER -->
    </activity>
</application>
```

Con eso, `io.github.sergioribera.istmo`:

- **compila el crate** con `cargo build --target <triple> --profile <p>`
  para cada ABI de `abiFilters` (default `arm64-v8a` + `x86_64`) y cada
  build type (`debug` → `dev`, el resto → `release`), y empaqueta
  `lib<crate>.so`. Usa el clang del NDK como linker y como `CC` / `AR`
  para build scripts basados en `cc`, salvo que hayas exportado los
  tuyos;
- **linkea cada plugin** del que depende el crate (del workspace, de
  crates.io o de git) usando `android/.istmo/istmo.json`, que escribe
  `build.rs`: fuentes Kotlin, manifests, recursos, dependencias
  `[[gradle]]` y el chequeo de `[min_versions] android`;
- **aplica `[app]`**: `applicationId`, `versionName`, `versionCode`,
  `minSdk`, `${istmoLabel}`, `${istmoIcon}`. Si Gradle tiene otro valor,
  se reemplaza con un aviso.

Si `istmo.json` falta, es más viejo que `Cargo.toml`, `Cargo.lock` o
`istmo.toml`, o nombra paths que no existen en este checkout, el plugin
lo regenera con `cargo check --lib --target <primer ABI>` antes de
configurar. Si cargo cambia el set de plugins durante un build, el build
se detiene y te pide volver a correrlo. AGP 8.3+ fusiona los manifests
de los plugins por variante; los AGP más viejos reciben un manifest
overlay combinado por build type.

Configuración opcional:

```kotlin
istmo {
    profiles.put("debug", "release")   // perfil de cargo por build type
    cargoArgs.addAll("--features", "extra")
    defaultAbis.set(listOf("arm64-v8a"))
    cargo.set("/opt/rust/bin/cargo")    // default: PATH, después $CARGO_HOME/bin, después ~/.cargo/bin
    crateDir.set(file("../rust"))       // default: el padre de la raíz de Gradle
}
```

## iOS

`build.rs` escribe `istmo-plugins.yml` al lado de las fuentes de tu app,
y dentro de `ios/.istmo/`:

- `Istmo.xcconfig`: bundle id, `MARKETING_VERSION`,
  `CURRENT_PROJECT_VERSION`, `ISTMO_DISPLAY_NAME`, deployment target,
  set de íconos y los settings para linkear la librería Rust;
- `build-rust.sh`: la fase pre-build; corre cargo para cada
  arquitectura que compila Xcode y deja `lib<crate>.a` en su lugar;
- `Assets.xcassets/IstmoAppIcon.appiconset` cuando `[app.icon] image` está
  seteado;
- `Assets/`: todas las entradas `[[assets]]` de la app y de los plugins
  (las de plugins bajo `Assets/plugin-<id>/`), empaquetadas como una
  sola folder reference, así quedan en `<bundle>/Assets/…`, donde las lee
  `istmo::assets`.

Con xcodegen, incluye el fragmento: trae las fuentes de los plugins, los
mismos build settings, la fase pre-build y la carpeta de assets:

```yaml
# ios/project.yml
name: MyApp
include:
  - path: MyApp/istmo-plugins.yml
targets:
  MyApp:
    type: application
    platform: iOS
    sources: [MyApp]
    info: { path: MyApp/Info.plist }
    dependencies:
      - package: IstmoRuntime
```

Los settings de `project.yml` le ganan a los del fragmento, así que
saca de ahí `PRODUCT_BUNDLE_IDENTIFIER`, `OTHER_LDFLAGS` y compañía. En
un proyecto Xcode hecho a mano, basa el target en `Istmo.xcconfig` y
agrega `"${PROJECT_DIR}/.istmo/build-rust.sh"` como fase Run Script; si
la app tiene assets, agrega `.istmo/Assets` al target como folder
reference (no como grupo).

El fragmento también declara bajo `packages:` los paquetes SwiftPM que
piden tus plugins y tu propio `istmo.toml` (`[[swift_package]]`) y
enlaza sus productos en el target de la app, así que no hay que
agregar nada a mano; un paquete que `project.yml` declare con el mismo
nombre conserva su `url` / versión. Un proyecto de Xcode hecho a mano
agrega esos paquetes con File → Add Package Dependencies.

En `Info.plist`, referencia los valores generados:

```xml
<key>CFBundleDisplayName</key>        <string>$(ISTMO_DISPLAY_NAME)</string>
<key>CFBundleShortVersionString</key> <string>$(MARKETING_VERSION)</string>
<key>CFBundleVersion</key>            <string>$(CURRENT_PROJECT_VERSION)</string>
```

`MARKETING_VERSION` descarta cualquier sufijo pre-release
(`0.3.0-beta.1` → `0.3.0`), como exige App Store Connect.

Los archivos generados se regeneran en cada build; después de clonar,
corre un `cargo build` (cualquier target) antes de `xcodegen generate`.

## Doctor

Los errores de setup se reportan donde ya estás mirando, con el arreglo
explicado:

- **`cargo build`**: `build.rs` imprime avisos `istmo doctor:`: falta el
  crate type `cdylib` / `staticlib`, `android/app` sin `io.github.sergioribera.istmo`,
  un manifest que ignora `${istmoLabel}` / `${istmoIcon}`, un
  `project.yml` sin el fragmento, un `Info.plist` con la versión
  hardcodeada. Una clave de `istmo.toml` mal escrita o con el tipo
  equivocado hace fallar el build.
- **Gradle**: antes de correr cargo, lo que falte hace fallar el build:
  `cargo` no encontrado (también cuando Android Studio arrancó sin el
  `PATH` de tu shell), target de Rust no instalado, sin NDK.
  `./gradlew istmoDoctor` imprime el reporte completo:

  ```text
  istmo doctor
    ✓ cargo: /home/me/.cargo/bin/cargo
    ✗ Rust target x86_64-linux-android is not installed
        → run `rustup target add x86_64-linux-android`
    ✓ linker for aarch64-linux-android: NDK 26.1.10909125
    ✓ android/.istmo/istmo.json: 2 plugin(s), app com.example.myapp
  1 issue(s) found.
  ```
- **Xcode**: `build-rust.sh` corta con una línea `error:` si falta
  cargo o el target de Rust.

## Qué commitear

Todo lo que genera `build.rs` se reconstruye en cada build. Los
directorios generados por completo llevan su propio `.gitignore` (con
`*`), así que git los ignora sin configurar nada:

```text
android/.istmo/
ios/.istmo/
android/app/src/main/java/dev/istmo/generated/
android/app/src/main/java/<namespace>/gen/      # paquete de codegen por defecto
android/app/src/main/java/<android_package del plugin>/
ios/<App>/Plugins/<T>/Generated/
```

Los archivos generados sueltos no quedan cubiertos y conviene
ignorarlos:

```text
ios/*/Plugins/IstmoMain.swift
ios/*/Plugins/IstmoPluginRegistry.swift
```

Un directorio que compartes con código escrito a mano (un
`android_package` explícito, un `<T>BackendImpl` sembrado) nunca se
marca.

Commitea `istmo-plugins.yml` si tu `project.yml` lo incluye y quieres
que `xcodegen generate` funcione antes del primer build de cargo.
