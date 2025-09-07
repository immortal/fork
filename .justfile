test: clippy
  cargo test

clippy:
  cargo clippy --all-targets --all-features -- -D warnings
