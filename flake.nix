{
  description = "Development environment for the Pomme client";

  inputs = {
    nixpkgs.url = "https://channels.nixos.org/nixpkgs-unstable/nixexprs.tar.zst";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs:
    let
      forAllSystems = inputs.nixpkgs.lib.genAttrs [
        "x86_64-linux"
        "aarch64-linux"
      ];
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ (import inputs.rust-overlay) ];
          };

          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

          runtimeLibs = with pkgs; [
            vulkan-loader
            wayland
            libxkbcommon
            libx11
            libxcursor
            libxi
            libxrandr
            libxcb
            alsa-lib
            udev
            dbus
            openssl
          ];
        in
        {
          default = pkgs.mkShell {
            packages = [
              rustToolchain
              pkgs.just
              pkgs.cargo-nextest
              pkgs.pkg-config
              pkgs.cmake
              pkgs.ninja
              pkgs.python3
              pkgs.vulkan-headers
            ]
            ++ runtimeLibs;

            RUSTFLAGS = "-L native=${pkgs.vulkan-loader}/lib";
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibs;
          };
        }
      );
    };
}
