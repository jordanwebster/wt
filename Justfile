verify: lint test fmt-check

lint:
    cargo clippy --all-targets -- -D warnings

test:
    cargo test

fmt-check:
    cargo fmt -- --check

# Checks that an older wt lists and removes a tree that has the files newer
# ones write (holds, pins, owners). The default is the last commit before
# them; pass the last release's tag once there is one.
older-wt ref="9b73ee2":
    #!/bin/sh
    set -eu
    source="$(mktemp -d)"
    trap 'rm -rf "$source"' EXIT
    git archive "{{ref}}" | tar -x -C "$source"
    cargo build --quiet --manifest-path "$source/Cargo.toml" --target-dir target/older-wt
    WT_OLDER_BIN="$PWD/target/older-wt/debug/wt" cargo test --test holds an_older_wt -- --nocapture
