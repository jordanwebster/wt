verify: lint test fmt-check

lint:
    cargo clippy --all-targets -- -D warnings

test:
    cargo test

fmt-check:
    cargo fmt -- --check
