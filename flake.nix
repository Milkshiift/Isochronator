{
  description = "Isochronator dev shell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, fenix, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        rustToolchain = fenix.packages.${system}.latest.withComponents [
          "rustc"
          "cargo"
          "rust-src"
          "rustfmt"
        ];
      in
      {
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            pkg-config
            rustToolchain
            fish
            fenix.packages.${system}.rust-analyzer
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
            mold
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
            mold
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
            mold
          ];

          VK_LAYER_PATH = "${pkgs.vulkan-validation-layers}/share/vulkan/explicit_layer.d";

          shellHook = ''
            export RUST_SRC_PATH="${rustToolchain}/lib/rustlib/src/rust/library"
            exec fish
          '';
        };
      }
    );
}
