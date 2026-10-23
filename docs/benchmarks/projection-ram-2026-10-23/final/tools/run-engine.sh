#!/bin/bash
set -euo pipefail
export PATH=/home/rocket2/.sa-agents/oojuro-toolchain/bin:/home/rocket2/.cargo/bin:/usr/bin:/bin
export CC=/home/rocket2/Documents/Codex/2026-10-22/read-engineering-github-project-and-then/work/portable-key-build/cc
export CXX=/home/rocket2/Documents/Codex/2026-10-22/read-engineering-github-project-and-then/work/portable-key-build/c++
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="$CC"
export RUSTFLAGS='-C target-cpu=x86-64'
export CARGO_BUILD_JOBS=2
export CARGO_TARGET_DIR=/home/rocket2/Documents/Codex/2026-10-22/read-engineering-github-project-and-then/work/portable-benchmark/target
exec cargo "$@"
