build:
    @cargo build --quiet --release -Zunstable-options --artifact-dir out

test *FILTER: build
    @cargo valo test {{ FILTER }}

test-update *FILTER: build
    @cargo valo test --update {{ FILTER }}
