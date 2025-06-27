{
  description = "Isochronator dev shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
      in
      {
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            pkg-config
            (rust-bin.selectLatestNightlyWith (toolchain: toolchain.default))
            fish
          ];

          buildInputs = with pkgs; [
            xorg.libX11
            xorg.libX11.dev
            xorg.libXcursor
            xorg.libXrandr
            xorg.libXi
            wayland
            wayland.dev
            libxkbcommon
            libGL
            alsa-lib
            udev
            vulkan-loader
            vulkan-headers
            vulkan-tools
          ];

          PKG_CONFIG_PATH = with pkgs; lib.makeSearchPath "lib/pkgconfig" [
            xorg.libX11.dev
            xorg.libXcursor.dev
            xorg.libXrandr.dev
            xorg.libXi.dev
            wayland.dev
            libxkbcommon
            alsa-lib.dev
            vulkan-loader
          ];

          LD_LIBRARY_PATH = with pkgs; lib.makeLibraryPath [
            xorg.libX11
            xorg.libXcursor
            xorg.libXrandr
            xorg.libXi
            wayland
            libxkbcommon
            libGL
            alsa-lib
            udev
            vulkan-loader
          ];

          VK_LAYER_PATH = "${pkgs.vulkan-validation-layers}/share/vulkan/explicit_layer.d";

          shellHook = ''
            exec fish
          '';
        };
      }
    );
}