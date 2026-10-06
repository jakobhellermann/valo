set positional-arguments

HK := `for p in "$HOME/.apps/Heroic/Hollow Knight/game/Hollow Knight" "$HOME/.local/share/Steam/steamapps/common/Hollow Knight/hollow_knight.x86_64"; do [ -f "$p" ] && { echo "$p"; break; }; done; true`

_default:
    just --list

valo *args:
    @cargo run -- "$@"

test *FILTER:
    @cargo valo test "$@"

run-sdl *args:
    make out/manual/sdl2-test
    just valo run {{ args }}"./out/manual/sdl2-test"

STEAM_WRAPPER := `if command -v steam-run >/dev/null 2>&1; then printf steam-run; fi`

run-hollowknight *args:
    {{ STEAM_WRAPPER }} just valo run {{ args }} "{{ HK }}" -force-gfx-direct

spawn-hollowknight *args:
    {{ STEAM_WRAPPER }} just valo spawn --use-gpu {{ args }} "{{ HK }}" -force-gfx-direct -force-vulkan

run-hollowknight-gpu *args:
    {{ STEAM_WRAPPER }} just valo run --use-gpu {{ args }} "{{ HK }}" -force-gfx-direct -force-vulkan

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

clean:
    rm -fr /tmp/valo* /tmp/tmp-valo-maps "$HOME/.local/state/valo"

# reproduce the current bug
repro XTRACE="": clean
    #!/usr/bin/env bash
    set -euo pipefail
    cmd="vkcube"
    pkill "$cmd" || true
    display="${DISPLAY:-:1}"
    if [[ -n "{{ XTRACE }}" ]]; then
        pkill xtrace || true
        rm -f /tmp/xtrace.log
        nix run nixpkgs#xtrace -d :1 -D :9 -n -k -o /tmp/xtrace.log > /tmp/xtrace-start.log 2>&1 &
        sleep 0.5
        display=":9"
    fi
    just valo run --max-restores 1 --checkpoint-frame 60 --restore-frame 120 "$cmd"
