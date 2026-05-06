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
    just valo run "./out/manual/sdl2-test" "$@"

run-hollowknight *args: build
    # VALO_LOG=/dev/stderr just valo run "/home/jakob/.apps/Heroic/Hollow Knight/game/Hollow Knight" -force-gfx-direct
    just valo run "$@" "/home/jakob/.apps/Heroic/Hollow Knight/game/Hollow Knight" -force-gfx-direct
