build:
    cargo build

check:
    cargo check

lint: clippy fmt sort

clippy:
    cargo clippy --all-targets -- -D warnings
    cargo machete

fmt:
    cargo fmt --check

sort:
    cargo sort --check

lint-fix: clippy-fix fmt-fix sort-fix

clippy-fix:
    cargo clippy --all-targets --fix --allow-dirty -- -D warnings
    cargo machete --fix

fmt-fix:
    cargo fmt

sort-fix:
    cargo sort
