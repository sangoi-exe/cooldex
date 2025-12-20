COMMON_MISTAKES
==============

2025-12-20 – Rustfmt / rustup temp dir permission denied

Wrong command: `cd codex-rs && just fmt`
Cause and fix: In this sandbox, rustup may fail to create temp files under `~/.rustup/tmp` with “Permission denied”. Set `RUSTUP_HOME` and `CARGO_HOME` to a writable location (e.g. `~/.codextools`) and rerun.
Correct command: `cd codex-rs && RUSTUP_HOME=$HOME/.codextools/rustup CARGO_HOME=$HOME/.codextools/cargo just fmt`

