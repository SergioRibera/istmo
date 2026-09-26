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
| Plugin de Gradle `dev.istmo.app` | en cada build de Gradle | compila el crate Rust, linkea plugins, aplica `[app]` |
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
icon    = "assets/icon.png"     # íconos de launcher para ambas plataformas
# version = "1.2.0"             # default: `package.version` de Cargo.toml

[min_versions]
android = 24                    # minSdk
ios     = "15.0"                # IPHONEOS_DEPLOYMENT_TARGET
```

Todas las claves son opcionales. Omití `id` cuando Android e iOS
necesitan identificadores distintos: los proyectos nativos conservan los
suyos.

## Android

Aplicá el plugin al módulo de la app:

```kotlin
// android/app/build.gradle.kts
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("dev.istmo.app")
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

y referenciá los placeholders desde el manifest:

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

Con eso, `dev.istmo.app`:

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

Si `istmo.json` falta o es más viejo que `Cargo.toml`, `Cargo.lock` o
`istmo.toml`, el plugin lo regenera con `cargo check` antes de
configurar. Si cargo cambia el set de plugins durante un build, el build
se detiene y te pide volver a correrlo.

Configuración opcional:

```kotlin
istmo {
    profiles.put("debug", "release")   // perfil de cargo por build type
    cargoArgs.addAll("--features", "extra")
    defaultAbis.set(listOf("arm64-v8a"))
    cargo.set("/opt/rust/bin/cargo")    // default: PATH, después ~/.cargo/bin
    crateDir.set(file("../rust"))       // default: el padre de la raíz de Gradle
}
```

`dev.istmo.app` reemplaza a `dev.istmo.plugin-loader`, que sólo
linkeaba plugins miembros del mismo workspace de Cargo.

## iOS

`build.rs` escribe `istmo-plugins.yml` al lado de las fuentes de tu app,
y dentro de `ios/.istmo/`:

- `Istmo.xcconfig`: bundle id, `MARKETING_VERSION`,
  `CURRENT_PROJECT_VERSION`, `ISTMO_DISPLAY_NAME`, deployment target,
  set de íconos y los settings para linkear la librería Rust;
- `build-rust.sh`: la fase pre-build; corre cargo para cada
  arquitectura que compila Xcode y deja `lib<crate>.a` en su lugar;
- `Assets.xcassets/IstmoAppIcon.appiconset` cuando `[app] icon` está
  seteado.

Con xcodegen, incluí el fragmento: trae las fuentes de los plugins, los
mismos build settings y la fase pre-build:

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
sacá de ahí `PRODUCT_BUNDLE_IDENTIFIER`, `OTHER_LDFLAGS` y compañía. En
un proyecto Xcode hecho a mano, basá el target en `Istmo.xcconfig` y
agregá `"${PROJECT_DIR}/.istmo/build-rust.sh"` como fase Run Script.

En `Info.plist`, referenciá los valores generados:

```xml
<key>CFBundleDisplayName</key>        <string>$(ISTMO_DISPLAY_NAME)</string>
<key>CFBundleShortVersionString</key> <string>$(MARKETING_VERSION)</string>
<key>CFBundleVersion</key>            <string>$(CURRENT_PROJECT_VERSION)</string>
```

`MARKETING_VERSION` descarta cualquier sufijo pre-release
(`0.3.0-beta.1` → `0.3.0`), como exige App Store Connect.

Los archivos generados se regeneran en cada build; después de clonar,
corré un `cargo build` (cualquier target) antes de `xcodegen generate`.

## Doctor

Los errores de setup se reportan donde ya estás mirando, con el arreglo
explicado:

- **`cargo build`**: `build.rs` imprime avisos `istmo doctor:`: falta el
  crate type `cdylib` / `staticlib`, `android/app` sin `dev.istmo.app`,
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

Todo lo que genera `build.rs` se reconstruye en cada build y se puede
ignorar:

```text
android/.istmo/
ios/.istmo/
android/app/src/main/java/dev/istmo/generated/
ios/*/Plugins/IstmoMain.swift
ios/*/Plugins/IstmoPluginRegistry.swift
```

Commiteá `istmo-plugins.yml` si tu `project.yml` lo incluye y querés
que `xcodegen generate` funcione antes del primer build de cargo.
