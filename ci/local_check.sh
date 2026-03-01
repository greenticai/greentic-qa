#!/usr/bin/env bash
set -euo pipefail

cargo fmt --all -- --check || { echo "cargo fmt --all -- --check failed" >&2; exit 1; }
tools/i18n.sh all || { echo "tools/i18n.sh all failed" >&2; exit 1; }
cargo clippy --workspace --all-targets -- -D warnings || { echo "cargo clippy --workspace --all-targets -- -D warnings failed" >&2; exit 1; }
cargo test --workspace || { echo "cargo test --workspace failed" >&2; exit 1; }

crates=(
  crates/qa-spec
  crates/component-qa
  crates/qa-lib
  crates/qa-cli
)

for crate in "${crates[@]}"; do
  if [[ "$crate" == "crates/qa-cli" ]]; then
    echo "Skipping package/publish dry-run for $crate until greentic-qa-lib is published."
    continue
  fi
  echo "Local dry-run publish for $crate"
  cargo package \
    --manifest-path "$crate/Cargo.toml" \
    --locked \
    --allow-dirty
  cargo publish \
    --manifest-path "$crate/Cargo.toml" \
    --dry-run \
    --locked \
    --allow-dirty
done
