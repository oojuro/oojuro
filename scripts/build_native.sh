#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Oojuro contributors
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail
[[ $# == 1 ]] || { printf 'Usage: scripts/build_native.sh /fresh/output\n' >&2; exit 2; }
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."
[[ -z "$(git status --porcelain)" ]] || { printf 'Commit reviewed source before building.\n' >&2; exit 1; }
target=x86_64-unknown-linux-gnu
# Cargo metadata/tree resolve the workspace before filtering, including uncompiled benchmarks.
cargo fetch --locked
cargo build --release --locked -p oojuro --bin oojuro --bin oojuro-native --target "$target"
python3 scripts/package_native.py "${CARGO_TARGET_DIR:-target}/$target/release" "$1" --target "$target"
