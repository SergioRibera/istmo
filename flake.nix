{
  description = "istmo — dev shell for desktop (egui) builds";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };

        # Libraries needed at both link time and runtime for the desktop
        # eframe demos (glow backend, Wayland + X11 support, wgpu on the
        # mobile crate's dev builds).
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
        ];
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = runtimeLibs;

          nativeBuildInputs = with pkgs; [
            pkg-config
            cmake
            wayland-scanner
          ];

          # rustup-managed toolchain in ~/.cargo/bin is used as-is; we only
          # supply the C libraries the crates link against. Add `LD_LIBRARY_PATH`
          # so the built binary can dlopen `libGL` / `libxkbcommon` at runtime.
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;

          shellHook = ''
            echo "istmo devshell — desktop deps loaded"
            echo "  cargo run -p data-store-demo"
          '';
        };
      });
}
