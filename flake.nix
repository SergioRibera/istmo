{
  description = "istmo — desktop dev shell + on-device bench build targets";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, flake-utils, fenix, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          # Android SDK + Gradle pull in Google licenses + proprietary
          # tooling (build-tools, platform-tools, NDK bits). Users have
          # to opt in by accepting them locally; we mark them here so
          # the eval doesn't fail outright.
          config = {
            allowUnfree = true;
            android_sdk.accept_license = true;
          };
        };

        # Libraries needed at both link time and runtime for the desktop
        # eframe / winit demos (glow backend, Wayland + X11 support, wgpu
        # on the mobile crate's dev builds).
        runtimeLibs = with pkgs; [
          libGL
          libxkbcommon
          wayland
          fontconfig
          vulkan-loader
          libx11
          libxcursor
          libxi
          libxrandr
          libinput
          udev
          # Camera plugin desktop backend (nokhwa → v4l2-sys-mit).
          v4l-utils
        ];

        # ------ Android support shared by both benches -----------------
        #
        # `androidenv.composeAndroidPackages` assembles exactly the SDK /
        # NDK / build-tools combination we need and nothing else. The
        # versions follow the bench project's `compileSdk = 35` and the
        # NDK Gradle picks up through `cargo-ndk`.
        androidComposition = pkgs.androidenv.composeAndroidPackages {
          platformVersions = [ "35" "34" ];
          buildToolsVersions = [ "34.0.0" "35.0.0" ];
          includeNDK = true;
          ndkVersions = [ "26.3.11579264" ];
          includeEmulator = false;
          includeSystemImages = false;
        };
        androidSdk = androidComposition.androidsdk;
        androidSdkRoot = "${androidSdk}/libexec/android-sdk";
        androidNdkRoot = "${androidSdkRoot}/ndk-bundle";

        # Rust toolchain with the Android cross-compilation targets.
        # Fenix is the thinnest layer that gives us pinned, overlayable
        # toolchains without pulling in a full rustup.
        rustAndroid = fenix.packages.${system}.combine [
          fenix.packages.${system}.stable.toolchain
          fenix.packages.${system}.targets.aarch64-linux-android.stable.rust-std
          fenix.packages.${system}.targets.armv7-linux-androideabi.stable.rust-std
          fenix.packages.${system}.targets.x86_64-linux-android.stable.rust-std
        ];

        # Environment every Android-touching shell / app needs. Isolates
        # the common exports in one attrset so the two devShells and the
        # two `apps` entries can all reuse them.
        androidEnv = ''
          export ANDROID_HOME="${androidSdkRoot}"
          export ANDROID_SDK_ROOT="${androidSdkRoot}"
          export ANDROID_NDK_HOME="${androidNdkRoot}"
          export ANDROID_NDK_ROOT="${androidNdkRoot}"
          # Android Gradle Plugin 8+ wants a 17+ JDK. `jdk17` is LTS.
          export JAVA_HOME="${pkgs.jdk17.home}"
          export PATH="$JAVA_HOME/bin:$PATH"
        '';

        # ------ bench-android: Rust cdylib + Gradle APK ---------------
        #
        # The Rust side is built with `cargo ndk`; the APK side runs
        # `./gradlew assembleRelease` with network access (Gradle fetches
        # Maven deps on first build). The resulting APK is dropped into
        # the example's own `android/app/build/outputs`. Not hermetic
        # across the Gradle half — nixpkgs does not yet have a general
        # Gradle-FOD wrapper — but repeatable enough for the device
        # bench.
        benchAndroidApp = pkgs.writeShellApplication {
          name = "build-bench-android";
          runtimeInputs = [
            pkgs.cargo-ndk
            pkgs.gradle
            rustAndroid
            androidSdk
            pkgs.jdk17
          ];
          text = ''
            ${androidEnv}
            cd "${toString ./.}/examples/bench-android"
            # One ABI is enough for the blog (arm64-v8a covers every
            # modern device). Pass `-t x86_64` to the invocation below
            # to add the emulator variant.
            cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build -p bench-android --release
            cd android
            ./gradlew :app:assembleRelease
            echo "APK: $(pwd)/app/build/outputs/apk/release/app-release-unsigned.apk"
          '';
        };

        # ------ bench-flutter: Flutter APK -----------------------------
        benchFlutterApp = pkgs.writeShellApplication {
          name = "build-bench-flutter";
          runtimeInputs = [
            pkgs.flutter
            androidSdk
            pkgs.jdk17
            pkgs.cmake
            pkgs.ninja
            pkgs.pkg-config
          ];
          text = ''
            ${androidEnv}
            cd "${toString ./.}/examples/bench-flutter"
            if [ ! -d android ]; then
              flutter create --org dev.istmo --project-name bench_flutter .
              echo "note: 'flutter create' regenerated android/ and ios/;"
              echo "merge our AndroidManifest + MainActivity + AppDelegate on top before re-running"
              exit 1
            fi
            flutter pub get
            dart run pigeon --input lib/bench_api.dart || true
            flutter build apk --release
            echo "APK: $(pwd)/build/app/outputs/flutter-apk/app-release.apk"
          '';
        };
      in
      {
        # ------ `nix run .#<name>` builds an APK ---------------------
        apps = {
          bench-android = {
            type = "app";
            program = "${benchAndroidApp}/bin/build-bench-android";
          };
          bench-flutter = {
            type = "app";
            program = "${benchFlutterApp}/bin/build-bench-flutter";
          };
        };

        # `nix build .#<name>` exposes the same scripts so users who
        # expect `build` instead of `run` get the same result; the
        # `./result` symlink then points at the wrapper rather than the
        # APK itself. The APK stays in-tree under the example's build
        # directory (Gradle writes there; a hermetic wrapper would need
        # a Gradle-FOD cache, which is a follow-up).
        packages = {
          bench-android = benchAndroidApp;
          bench-flutter = benchFlutterApp;
        };

        # ------ `nix develop .#<name>` --------------------------------
        devShells = {
          default = pkgs.mkShell {
            buildInputs = runtimeLibs;

            nativeBuildInputs = with pkgs; [
              pkg-config
              cmake
              wayland-scanner
              gcc
              # bindgen (pulled transitively by v4l2-sys-mit) needs libclang;
              # cc-rs (mozjpeg-sys, …) uses the gcc above.
              llvmPackages_latest.libclang.lib
              nasm
            ];

            # rustup-managed toolchain in ~/.cargo/bin is used as-is; we only
            # supply the C libraries the crates link against. Add `LD_LIBRARY_PATH`
            # so the built binary can dlopen `libGL` / `libxkbcommon` at runtime.
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
            LIBCLANG_PATH = "${pkgs.llvmPackages_latest.libclang.lib}/lib";

            shellHook = ''
              echo "istmo devshell — desktop deps loaded (libinput + wayland + x11)"
              echo "  cargo run -p data-store-demo"
              echo "  cargo run -p pen-demo         # stylus/pencil sample logger"
            '';
          };

          bench-android = pkgs.mkShell {
            nativeBuildInputs = [
              rustAndroid
              pkgs.cargo-ndk
              pkgs.gradle
              androidSdk
              pkgs.jdk17
            ];
            shellHook = ''
              ${androidEnv}
              echo "bench-android devshell: cargo-ndk + Gradle + Android SDK/NDK ready"
              echo "  cd examples/bench-android"
              echo "  cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build -p bench-android --release"
              echo "  (cd android && ./gradlew :app:installRelease)"
            '';
          };

          bench-flutter = pkgs.mkShell {
            nativeBuildInputs = [
              pkgs.flutter
              androidSdk
              pkgs.jdk17
              pkgs.cmake
              pkgs.ninja
              pkgs.pkg-config
            ];
            shellHook = ''
              ${androidEnv}
              echo "bench-flutter devshell: Flutter SDK + Android SDK/NDK ready"
              echo "  cd examples/bench-flutter"
              echo "  flutter create --org dev.istmo --project-name bench_flutter ."
              echo "  dart run pigeon --input lib/bench_api.dart"
              echo "  flutter build apk --release"
            '';
          };
        };
      });
}
