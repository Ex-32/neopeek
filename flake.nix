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
          # runtime-dlopen'd libs (pulse, alsa, vulkan loader, wayland,
          # xkbcommon, X11 client libs) visible under `nix run`.
          postInstall = ''
            wrapProgram $out/bin/neopeek \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath [
                pkgs.libpulseaudio pkgs.alsa-lib pkgs.vulkan-loader
                pkgs.wayland pkgs.libxkbcommon
                pkgs.libx11 pkgs.libxcursor pkgs.libxrandr pkgs.libxi
              ]}
          '';

          meta = with pkgs.lib; {
            description = "Modern sndpeek-style audio visualizer (3D waterfall, telemetry HUD)";
            license = with licenses; [ gpl3Only ];
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
            wayland libxkbcommon libx11 libxcursor libxrandr libxi
            vulkan-tools
          ];
        };
      });
}
