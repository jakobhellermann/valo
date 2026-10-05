{
  description = "valo - todo";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    crane.url = "github:ipetkov/crane";

    flake-utils.url = "github:numtide/flake-utils";

    mesaGpu = {
      url = "github:jakobhellermann/mesa/wip";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    gfxstream = {
      url = "github:jakobhellermann/gfxstream/wip";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    kumquat = {
      url = "github:jakobhellermann/rutabaga_gfx/wip";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.gfxstream.follows = "gfxstream";
    };

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      flake-utils,
      mesaGpu,
      kumquat,
      rust-overlay,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        # valo uses nightly features (sync_unsafe_cell, io_const_error, maybe_uninit_as_bytes)
        craneLib = (crane.mkLib pkgs).overrideToolchain (
          pkgs.rust-bin.nightly.latest.default.override {
            extensions = [
              "rust-src"
              "rust-analyzer"
            ];
          }
        );

        # cleanCargoSource strips .c/.h, but the valo build.rs compiles native/valo_shim.c
        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions [
            (craneLib.fileset.commonCargoSources ./.)
            ./crates/valo/native
          ];
        };

        commonArgs = {
          inherit src;
          strictDeps = true;
          # valo's cdylib links against X11 (and the shim compiles against its headers)
          buildInputs = [
            pkgs.libx11.dev
            pkgs.libxcb.dev
          ];
          # tests need the C fixtures from tests/, built via make with SDL/X11 and run under LD_PRELOAD
          doCheck = false;
          pname = "valo";
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        kumquatPackage = kumquat.packages.${system}.default;
        gfxstreamIcd = "${
          mesaGpu.packages.${system}.default
        }/share/vulkan/icd.d/gfxstream_vk_icd.x86_64.json";

        valo-client = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            pname = "valo-client";
            cargoExtraArgs = "--locked -p valo-client";
            nativeBuildInputs = [ pkgs.makeWrapper ];
            postInstall = ''
              ln -s ${valo}/lib/libvalo.so $out/bin/libvalo.so
            '';
            postFixup = ''
              wrapProgram $out/bin/valo \
                --prefix PATH : ${pkgs.lib.makeBinPath [ kumquatPackage ]} \
                --set-default VALO_GFXSTREAM_ICD ${gfxstreamIcd}
            '';
          }
        );

        valo = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            pname = "valo";
            cargoExtraArgs = "--locked -p valo";
          }
        );
      in
      {
        checks = {
          inherit valo valo-client;
        };

        packages = {
          default = valo-client;
          inherit valo valo-client;
        };

        apps.default = flake-utils.lib.mkApp {
          drv = valo-client;
          name = "valo";
        };

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};
          packages = with pkgs; [
            clang
            just
            perf
            perfetto
            SDL2.dev
            sdl3.dev
            vulkan-headers
            vulkan-loader
            mesaGpu.packages.${system}.default
            kumquatPackage
          ];
          shellHook = ''
            export VALO_GFXSTREAM_ICD=${gfxstreamIcd}
            # nix develop sets TMPDIR to a per-shell dir that doesn't exist in steam-run /tmp
            unset TMPDIR
          '';
        };
      }
    );
}
