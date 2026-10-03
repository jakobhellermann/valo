set positional-arguments

build:
    @cargo build --quiet --release -Zunstable-options --artifact-dir out -p valo -p valo-client

test *FILTER: build
    @cargo valo test "$@"

test-update *FILTER: build
    @cargo valo test --update "$@"

valo *args: build
    @./out/valo "$@"

run-sdl *args: build
    make out/manual/sdl2-test
    # VALO_LOG=/dev/stderr just valo run "./out/manual/sdl2-test"
    just valo run {{ args }}"./out/manual/sdl2-test"

run-hollowknight *args: build
    # VALO_LOG=/dev/stderr just valo run "/home/jakob/.apps/Heroic/Hollow Knight/game/Hollow Knight" -force-gfx-direct
    just valo run {{ args }} "/home/jakob/.apps/Heroic/Hollow Knight/game/Hollow Knight" -force-gfx-direct

save-repro:
    mkdir -p repro
    cp /tmp/valo.log ./repro
    coredumpctl dump > repro/coredump

visualizer:
    @cd visualizer && pnpm run dev

vendor:
    #!/usr/bin/env bash
    set -euo pipefail
    export LC_ALL=C
    version=1.17.0
    archive_sha256=599ebf9996710fea71622e6e184f3a8ad5b43d0e5fa8c4e407123c88a59a6d55
    archive_url="https://xorg.freedesktop.org/archive/individual/lib/libxcb-${version}.tar.xz"
    temp_dir="$(mktemp -d)"
    trap 'rm -rf "$temp_dir"' EXIT
    curl --fail --location --silent --show-error --output "$temp_dir/libxcb.tar.xz" "$archive_url"
    printf '%s  %s\n' "$archive_sha256" "$temp_dir/libxcb.tar.xz" | sha256sum --check --status
    tar -xOf "$temp_dir/libxcb.tar.xz" "libxcb-${version}/src/xcbint.h" > "$temp_dir/upstream-xcbint.h"
    {
        printf '// Source: %s (sha256: %s)\n' "$archive_url" "$archive_sha256"
        sed -e 's@#include "bigreq.h"@#include <xcb/bigreq.h>@' -e 's/[[:blank:]]*$//' "$temp_dir/upstream-xcbint.h"
    } > "$temp_dir/xcbint.h"
    cp crates/valo/native/valo_shim.c "$temp_dir/valo_shim.c"
    "${CC:-cc}" -DVALO_XCB_SIZE_PROBE "$temp_dir/valo_shim.c" -o "$temp_dir/xcb_size_probe"
    xcb_size="$("$temp_dir/xcb_size_probe")"
    [[ "$xcb_size" =~ ^[0-9]+$ ]]
    printf '// generated from `just vendor`\nconst XCB_CONN_BUF_SIZE: usize = %s;\n' "$xcb_size" > "$temp_dir/generated.rs"
    install -m 0644 "$temp_dir/xcbint.h" crates/valo/native/xcbint.h
    install -m 0644 "$temp_dir/generated.rs" crates/valo/src/hooks/xlib/generated.rs
