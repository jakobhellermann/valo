set positional-arguments

HK := `for p in "$HOME/.apps/Heroic/Hollow Knight/game/Hollow Knight" "$HOME/.local/share/Steam/steamapps/common/Hollow Knight/hollow_knight.x86_64"; do [ -f "$p" ] && { echo "$p"; break; }; done; true`

_default:
    just --list

valo *args:
    @cargo run --release -- "$@"

test *FILTER:
    @cargo valo test "$@"

# Profile checkpoint and restore
profile *ARGS:
    ./scripts/profile "{{ HK }}" "$@"

# Text summary: valo stage durations and, in GPU runs, the perfetto envelopes.
profile-summary:
    ./scripts/profile-summary

# Export Valo stages and all available perf call stacks on one Perfetto timeline.
profile-perfetto:
    python3 scripts/perfetto-export > out/perf-profile.tar

# Open a capture in perf report (default: restore).
profile-report STAGE="restore":
    #!/usr/bin/env bash
    set -euo pipefail
    stage="$1"
    data="out/perf-$stage.data"
    [[ -s "$data" ]] || { echo "Run 'just profile' first" >&2; exit 1; }
    perf report -i "$data"

# Serve the perfetto trace for the UI: open https://ui.perfetto.dev/?local=1
# and allow the local trace processor on 127.0.0.1:9001. Ctrl+C stops.
profile-pftrace:
    #!/usr/bin/env bash
    set -euo pipefail
    [[ -s out/gpu-restore.pftrace ]] || { echo "missing out/gpu-restore.pftrace (GPU profile run?)" >&2; exit 1; }
    command -v trace_processor_shell >/dev/null || { echo "trace_processor_shell not in PATH (nix-config perfetto-tools)" >&2; exit 1; }
    echo "open https://ui.perfetto.dev/?local=1 and allow the local trace processor"
    exec trace_processor_shell --httpd out/gpu-restore.pftrace

# Open a capture in Samply; valid stages are whatever out/*.jslb.gz exists.
profile-samply STAGE="":
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -z "{{ STAGE }}" ]]; then
        echo "stages:"
        ls out/*.jslb.gz 2>/dev/null | sed 's|out/perf-||; s|\.jslb\.gz||' || echo "  (none — run 'just profile' with samply in PATH)"
        exit 0
    fi
    data="out/perf-{{ STAGE }}.jslb.gz"
    if ! [[ -s "$data" ]]; then
        echo "missing $data; available:" >&2
        ls out/*.jslb.gz 2>/dev/null >&2 || echo "  (none — run 'just profile' with samply in PATH)" >&2
        exit 1
    fi
    samply load "$data"

run-sdl *args:
    make out/manual/sdl2-test
    just valo run {{ args }}"./out/manual/sdl2-test"

STEAM_WRAPPER := `if command -v steam-run >/dev/null 2>&1; then printf steam-run; fi`

run-hollowknight *args:
    {{ STEAM_WRAPPER }} just valo run {{ args }} "{{ HK }}"

spawn-hollowknight *args:
    {{ STEAM_WRAPPER }} just valo spawn {{ args }} "{{ HK }}"

run-silksong *args:
    {{ STEAM_WRAPPER }} just valo run {{ args }} "$HOME/.steamapps/Hollow Knight Silksong/Hollow Knight Silksong"

run-silksong-modded *args:
    VALO_TARGET='Hollow Knight Silksong' just valo run --use-gpu --uncontrolled {{ args }} gust run -- -force-vulkan -force-gfx-direct

# Drive Hollow Knight from a replay script under valo (GPU mode, through
# steam-run). VALO_HOME pins the run dir so savestate slots survive reruns;
# {{ args }} are extra driver flags before the script.
replay-repro SCRIPT="repro/hk-menu.txt" *args:
    #!/usr/bin/env bash
    set -euo pipefail
    [[ -f "{{ SCRIPT }}" ]] || { echo "script {{ SCRIPT }} not found" >&2; exit 1; }
    make out/libvalo.so
    VALO_HOME="${VALO_HOME:-$HOME/.local/state/valo/repro}" {{ STEAM_WRAPPER }} cargo run --release -q -p valo-replay -- --use-gpu --lib out/libvalo.so {{ args }} "{{ SCRIPT }}" "{{ HK }}"

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
    rm -fr /tmp/valo* /tmp/tmp-valo-maps "$HOME/.local/state/valo" out/

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
        nix run nixpkgs#xtrace -- -d "$display" -D :9 -n -k -o /tmp/xtrace.log > /tmp/xtrace-start.log 2>&1 &
        for _ in $(seq 100); do [[ -S /tmp/.X11-unix/X9 ]] && break; sleep 0.1; done
        [[ -S /tmp/.X11-unix/X9 ]] || { echo "xtrace did not come up, see /tmp/xtrace-start.log" >&2; exit 1; }
        display=":9"
    fi
    DISPLAY="$display" just run-hollowknight-gpu --uncontrolled
