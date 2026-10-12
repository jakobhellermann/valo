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

        appimageRuntime = pkgs.fetchurl {
          url = "https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64";
          sha256 = "0396xi3km1yd0z4329lbjxmv82ddc40gd0x8hca0ylcj7i28pjig";
        };

        appimage =
          let
            mesaGpuPackage = mesaGpu.packages.${system}.default;
            glibcFloor = "2.43";
          in
          pkgs.stdenv.mkDerivation {
            pname = "valo-appimage";
            inherit (valo-client) version;

            dontUnpack = true;
            dontStrip = true;

            nativeBuildInputs = with pkgs; [
              patchelf
              binutils
              squashfsTools
              jq
              file
            ];

            buildCommand = ''
              appdir=$PWD/AppDir
              libdir=$appdir/usr/lib
              mkdir -p $appdir/usr/bin $libdir $appdir/usr/share/vulkan/icd.d

              install -m755 ${valo-client}/bin/.valo-wrapped $appdir/usr/bin/valo
              install -m755 ${valo}/lib/libvalo.so $appdir/usr/bin/libvalo.so
              install -m755 ${kumquatPackage}/bin/.kumquat-wrapped $appdir/usr/bin/kumquat

              # kumquat's gfxstream backend dlopens libvulkan.so.1
              install -m755 ${mesaGpuPackage}/lib/libvulkan_gfxstream.so $libdir/libvulkan_gfxstream.so
              install -m755 ${pkgs.vulkan-loader}/lib/libvulkan.so.1 $libdir/libvulkan.so.1

              elfs() { find $appdir/usr/bin $libdir -type f; }

              changed=1
              while [ "$changed" -ne 0 ]; do
                changed=0
                for elf in $(elfs); do
                  deps=$(ldd "$elf")
                  while read -r name arrow path rest; do
                    [ "$arrow" = "=>" ] || continue
                    case "$path" in
                      not)
                        echo "unresolved dependency of $elf: $name" >&2
                        exit 1
                        ;;
                      /nix/store/*-glibc-*) continue ;;
                      /nix/store/*) ;;
                      *) continue ;;
                    esac
                    [ -e "$libdir/$name" ] && continue
                    cp -L "$path" "$libdir/$name"
                    changed=1
                  done <<< "$deps"
                done
              done
              chmod -R u+w "$appdir"

              for elf in $(elfs); do
                file -b "$elf" | grep -q interpreter || continue
                patchelf --set-interpreter /lib64/ld-linux-x86-64.so.2 "$elf"
              done
              for elf in $appdir/usr/bin/*; do
                patchelf --set-rpath '$ORIGIN/../lib' "$elf"
              done
              for elf in $libdir/*; do
                patchelf --set-rpath '$ORIGIN' "$elf"
              done

              # glibc compatibility check
              for elf in $(elfs); do
                info=$(readelf -W -V "$elf")
                needs=$(awk '/Name: GLIBC_/ && !/Flags: WEAK/ {print $3}' <<< "$info" | sort -Vu)
                for need in $needs; do
                  case "$need" in
                    GLIBC_[0-9]*) v=''${need#GLIBC_} ;;
                    GLIBC_ABI_GNU2_TLS) v=2.43 ;;
                    *) v= ;;
                  esac
                  [ -n "$v" ] && [ "$(printf '%s\n${glibcFloor}\n' "$v" | sort -V | tail -1)" = "${glibcFloor}" ] && continue
                  echo "glibc floor ${glibcFloor} violated: $elf needs $need" >&2
                  exit 1
                done
              done

              api_version=$(jq -r .ICD.api_version ${gfxstreamIcd})
              cat > $appdir/usr/share/vulkan/icd.d/gfxstream_vk_icd.x86_64.json <<JSON
              {
                "file_format_version": "1.0.1",
                "ICD": {
                  "api_version": "$api_version",
                  "library_path": "../../../lib/libvulkan_gfxstream.so"
                }
              }
              JSON

              ln -s usr/bin/valo $appdir/AppRun
              cp ${./packaging/icon.png} $appdir/valo.png
              ln -s valo.png $appdir/.DirIcon
              cat > $appdir/valo.desktop <<DESKTOP
              [Desktop Entry]
              Type=Application
              Name=valo
              Exec=valo
              Icon=valo
              Terminal=true
              Categories=Development;
              DESKTOP

              mksquashfs \
                "$appdir/AppRun" "$appdir/usr" "$appdir/valo.desktop" \
                "$appdir/valo.png" "$appdir/.DirIcon" \
                squashfs.img \
                -all-root -noappend -no-recovery -comp zstd
              cat ${appimageRuntime} squashfs.img > $out
              chmod 755 $out
            '';
          };
      in
      {
        checks = {
          inherit valo valo-client;
        };

        packages = {
          default = valo-client;
          inherit valo valo-client appimage;
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
