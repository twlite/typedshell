#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

cargo run --quiet --locked --manifest-path "$repo_root/Cargo.toml" --bin tsh -- \
  compile "$repo_root/scripts/install.tsh" --target bash --os linux --print \
  > "$repo_root/scripts/install.sh"

cargo run --quiet --locked --manifest-path "$repo_root/Cargo.toml" --bin tsh -- \
  compile "$repo_root/scripts/install.tsh" --target pwsh --os windows --print \
  > "$repo_root/scripts/install.ps1"
