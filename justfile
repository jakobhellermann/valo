set positional-arguments := true

build:
    @cargo build --quiet --release -Zunstable-options --artifact-dir out

test *FILTER: build
    @cargo valo test "$@"

test-update *FILTER: build
    @cargo valo test --update "$@"

valo *args: build
    @./out/valo "$@"
