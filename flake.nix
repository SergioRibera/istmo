{
  description = "istmo — dev shell for desktop (egui / winit) builds";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };

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
      in
      {
        devShells.default = pkgs.mkShell {
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
      });
}
