{
  description = "neopeek: modern sndpeek-style audio visualizer";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        neopeek = pkgs.rustPlatform.buildRustPackage {
          pname = "neopeek";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [ pkgs.pkg-config pkgs.makeWrapper ];
          buildInputs = with pkgs; [ libpulseaudio alsa-lib vulkan-loader ];

          # makeWrapper.pc is provided in stdenv hooks; wrapProgram keeps
          # libpulse/alsa/vulkan loader paths visible under `nix run`.
          postInstall = ''
            wrapProgram $out/bin/neopeek \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath [
                pkgs.libpulseaudio pkgs.alsa-lib pkgs.vulkan-loader
              ]}
          '';

          meta = with pkgs.lib; {
            description = "Modern sndpeek-style audio visualizer (3D waterfall, telemetry HUD)";
            license = with licenses; [ mit asl20 ];
            platforms = platforms.linux;
            mainProgram = "neopeek";
          };
        };
      in
      {
        packages.default = neopeek;
        apps.default = { type = "app"; program = "${neopeek}/bin/neopeek"; };
        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo rustc rustfmt clippy
            pkg-config
            libpulseaudio alsa-lib vulkan-loader
            vulkan-tools
          ];
        };
      });
}
