set working-directory := "codex-rs"
set positional-arguments

export CODEX_REPO_ROOT := justfile_directory()
export JUST_SHELL := justfile_directory() / "scripts/just-shell.py"

set shell := ["python3", "-c", 'import os, runpy; runpy.run_path(os.environ["JUST_SHELL"], run_name="__main__")']
set windows-shell := ["python", "-c", 'import os, runpy; runpy.run_path(os.environ["JUST_SHELL"], run_name="__main__")']

rust_min_stack := "8388608"
python := if os_family() == "windows" { "python" } else { "python3" }

# Merge-safety anchor: build-like Cargo just recipes route through
# scripts/cargo-guard.sh so resource limits, cleanup, and receipts stay centralized.

# Display help
help:
    just -l

# `codex`

alias c := codex

codex *args:
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-build}" bash ../scripts/cargo-guard.sh cargo run --bin codex -- {args}

# `codex exec`
exec *args:
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-build}" bash ../scripts/cargo-guard.sh cargo run --bin codex -- exec {args}

# Start `codex exec-server` and run codex-tui.
[no-cd]
[positional-arguments]
[unix]
tui-with-exec-server *args:
    {{ justfile_directory() }}/scripts/run_tui_with_exec_server.sh "$@"

# Run the CLI version of the file-search crate.
file-search *args:
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-build}" bash ../scripts/cargo-guard.sh cargo run --bin codex-file-search -- {args}

# Run the standalone code-mode host from source.
code-mode-host *args:
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-build}" bash ../scripts/cargo-guard.sh cargo run --bin codex-code-mode-host -- {args}

# Assemble a local Codex package.
[no-cd]
assemble-codex-package *args:
    {{ python }} {{ justfile_directory() }}/scripts/build_codex_package.py {args}

# Build the CLI and run the app-server test client
app-server-test-client *args:
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-build}" bash ../scripts/cargo-guard.sh cargo build --target-dir ./target -p codex-cli --bin codex
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-build}" bash ../scripts/cargo-guard.sh cargo run -p codex-app-server-test-client -- --codex-bin ./target/debug/codex {args}

# Format the justfile, Rust, Bazel/Starlark, Python SDK code, and Python scripts.
fmt:
    @{{ python }} ../scripts/format.py

# Check formatting without modifying files.
fmt-check:
    @{{ python }} ../scripts/format.py --check

fix *args:
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-clippy}" bash ../scripts/cargo-guard.sh cargo clippy --fix --tests --allow-dirty {args}

clippy *args:
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-clippy}" bash ../scripts/cargo-guard.sh cargo clippy --tests {args}

clippy-strict *args:
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-clippy}" bash ../scripts/cargo-guard.sh cargo clippy {args} -- -D warnings

check-strict *args:
    RUSTFLAGS="${RUSTFLAGS:+${RUSTFLAGS} }-D warnings" \
        CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-check}" bash ../scripts/cargo-guard.sh cargo check {args}

[unix]
install:
    rustup show active-toolchain
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-build}" bash ../scripts/cargo-guard.sh cargo fetch

[windows]
install:
    #!powershell.exe -File
    $pwsh = Get-Command pwsh.exe -ErrorAction SilentlyContinue
    if (-not $pwsh) {
        winget install --exact --id Microsoft.PowerShell --source winget --accept-package-agreements --accept-source-agreements
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }
    rustup show active-toolchain
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    cargo fetch
    exit $LASTEXITCODE

# Run nextest through the checked-in resource and nextest profiles.
#
# Install cargo-nextest externally if it is not already available.
# Prefer this for routine local runs. Workspace crate features are banned, so

# there should be no need to add `--all-features`.
[unix]
test *args:
    NEXTEST_PROFILE="${NEXTEST_PROFILE:-local-safe}"; \
    resource_profile="${CARGO_GUARD_RESOURCE_PROFILE:-workspace_nextest}"; \
    if [ "$NEXTEST_PROFILE" = "local-disk-tight" ] && [ -z "${CARGO_GUARD_RESOURCE_PROFILE:-}" ]; then \
        resource_profile="workspace_nextest_tight"; \
    fi; \
    RUST_MIN_STACK={{ rust_min_stack }} CARGO_GUARD_RESOURCE_PROFILE="$resource_profile" bash ../scripts/cargo-guard.sh cargo nextest run --profile "$NEXTEST_PROFILE" {args}

[windows]
test *args:
    $env:RUST_MIN_STACK = "{{ rust_min_stack }}"; $env:NEXTEST_PROFILE = "local"; cargo nextest run --no-fail-fast @($args | Select-Object -Skip 1)

validate *args:
    ../scripts/cargo-guard.sh verify --changed --mode standard {args}

validate-resume *args:
    ../scripts/cargo-guard.sh verify --changed --mode standard --resume --explain-skip {args}

validate-partial-from index *args:
    @echo "PARTIAL / NOT FINAL VALIDATION: rerunning validation tail from command {{ index }} only."
    ../scripts/cargo-guard.sh verify --changed --mode standard --from-index "{{ index }}" --explain-skip {args}

validate-partial-failed *args:
    @echo "PARTIAL / NOT FINAL VALIDATION: rerunning only previously failed validation commands."
    ../scripts/cargo-guard.sh verify --changed --mode standard --only-failed --explain-skip {args}

validate-strict *args:
    ../scripts/cargo-guard.sh verify --changed --mode strict {args}

validate-strict-resume *args:
    ../scripts/cargo-guard.sh verify --changed --mode strict --resume --explain-skip {args}

validate-strict-partial-from index *args:
    @echo "PARTIAL / NOT FINAL VALIDATION: rerunning strict validation tail from command {{ index }} only."
    ../scripts/cargo-guard.sh verify --changed --mode strict --from-index "{{ index }}" --explain-skip {args}

validate-strict-partial-failed *args:
    @echo "PARTIAL / NOT FINAL VALIDATION: rerunning only previously failed strict validation commands."
    ../scripts/cargo-guard.sh verify --changed --mode strict --only-failed --explain-skip {args}

validate-plan *args:
    ../scripts/cargo-guard.sh plan --changed --mode standard {args}

validate-cli:
    ../scripts/cargo-guard.sh verify --surface cli --mode strict

build-codex-bin:
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-build}" bash ../scripts/cargo-guard.sh cargo build -p codex-cli --bin codex

check-codex-bin:
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-check}" bash ../scripts/cargo-guard.sh cargo check -p codex-cli --bin codex
    just check-strict -p codex-cli --bin codex

strict-codex-bin:
    just clippy-strict -p codex-cli --bin codex
    just check-strict -p codex-cli --bin codex

smoke-codex-bin:
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-build}" bash ../scripts/cargo-guard.sh cargo build -p codex-cli --bin codex
    target_dir="$(bash ../scripts/cargo-guard.sh cargo metadata --format-version=1 --no-deps --quiet | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')" && \
        "$target_dir/debug/codex" --version && \
        "$target_dir/debug/codex" --help && \
        "$target_dir/debug/codex" exec --help && \
        "$target_dir/debug/codex" app-server --help

# Run from the repository root so scripts that resolve paths from `cwd` see

# the same layout they use in GitHub Actions.
[no-cd]
test-github-scripts:
    {{ python }} -m unittest discover -s {{ justfile_directory() }}/.github/scripts -p 'test_*.py'

# Run explicit workspace benchmark targets.
bench *args:
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-build}" bash ../scripts/cargo-guard.sh cargo bench --workspace --bench '*' {args}

# Run benchmark targets once to ensure they start successfully.
bench-smoke:
    just bench -- --test

# Run Bazel-backed end-to-end macrobenchmarks with optimized binaries.
bench-e2e:
    # Keep measured binaries comparable to production-style optimized builds.
    bazel test --compilation_mode=opt --cache_test_results=no --test_output=streamed //codex-rs:e2e-benchmarks

# Run Bazel-backed end-to-end macrobenchmarks once per case with release-like

# Rust cfg paths but fastbuild codegen.
bench-e2e-smoke:
    # Avoid optimizer cost because smoke runs only check that benchmarks work.
    # Compile target Rust code through the same release-only cfg paths as opt.
    # Compile exec-platform Rust tools through those release-only cfg paths too.
    bazel test --compilation_mode=fastbuild --@rules_rust//rust/settings:extra_rustc_flag=-Cdebug-assertions=no --@rules_rust//rust/settings:extra_exec_rustc_flag=-Cdebug-assertions=no --cache_test_results=no --test_output=streamed --test_arg=--test //codex-rs:e2e-benchmarks

# Build and run Codex from source using Bazel.
# On Unix, use `[no-cd]` and `--run_under="cd $PWD &&"` to ensure Bazel runs

# the command in the current working directory.
[no-cd]
[unix]
bazel-codex *args:
    bazel run //codex-rs/cli:codex --run_under="cd $PWD &&" -- "$@"

[windows]
bazel-codex *args:
    bazel run //codex-rs/cli:codex --run_under='cd /d "{{ invocation_directory_native() }}" &&' -- @($args | Select-Object -Skip 1)

# Build and run the standalone code-mode host from source using Bazel.
[no-cd]
[unix]
bazel-code-mode-host *args:
    bazel run //codex-rs/code-mode-host:codex-code-mode-host --run_under="cd $PWD &&" -- "$@"

[windows]
bazel-code-mode-host *args:
    bazel run //codex-rs/code-mode-host:codex-code-mode-host --run_under='cd /d "{{ invocation_directory_native() }}" &&' -- @($args | Select-Object -Skip 1)

[no-cd]
bazel-lock-update:
    bazel mod deps --lockfile_mode=update

[no-cd]
[unix]
bazel-lock-check:
    {{ justfile_directory() }}/scripts/check-module-bazel-lock.sh

[windows]
bazel-lock-check:
    bazel mod deps --lockfile_mode=error; if ($LASTEXITCODE -ne 0) { Write-Error "MODULE.bazel.lock is out of date. Run 'just bazel-lock-update' and commit the updated lockfile."; exit 1 }

bazel-test:
    bazel test --test_tag_filters=-argument-comment-lint //... --keep_going

[no-cd]
[unix]
bazel-clippy:
    bazel_targets="$({{ justfile_directory() }}/scripts/list-bazel-clippy-targets.sh)" && bazel build --config=clippy -- ${bazel_targets}

[no-cd]
[unix]
bazel-argument-comment-lint:
    bazel build --config=argument-comment-lint -- $({{ justfile_directory() }}/tools/argument-comment-lint/list-bazel-targets.sh)

build-for-release:
    bazel build //codex-rs/cli:release_binaries

# Run the MCP server
mcp-server-run *args:
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-build}" bash ../scripts/cargo-guard.sh cargo run -p codex-mcp-server -- {args}

# Regenerate the json schema for config.toml from the current config types.
write-config-schema:
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-build}" bash ../scripts/cargo-guard.sh cargo run -p codex-core --bin codex-write-config-schema

# Regenerate vendored app-server protocol schema artifacts.
write-app-server-schema *args:
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-build}" bash ../scripts/cargo-guard.sh cargo run -p codex-app-server-protocol --bin write_schema_fixtures -- {args}

[no-cd]
write-hooks-schema:
    CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-build}" bash {{ justfile_directory() }}/scripts/cargo-guard.sh cargo run --manifest-path {{ justfile_directory() }}/codex-rs/Cargo.toml -p codex-hooks --bin write_hooks_schema_fixtures

test-cargo-guard:
    bash ../scripts/test-cargo-guard.sh

test-cargo-validate:
    python3 ../scripts/test-cargo-validate.py

# Run the argument-comment Dylint checks across codex-rs.
[no-cd]
[unix]
argument-comment-lint *args:
    if [ "$#" -eq 0 ]; then \
      bazel build --config=argument-comment-lint -- $({{ justfile_directory() }}/tools/argument-comment-lint/list-bazel-targets.sh); \
    else \
      {{ justfile_directory() }}/tools/argument-comment-lint/run-prebuilt-linter.py "$@"; \
    fi

[no-cd]
argument-comment-lint-from-source *args:
    {{ python }} {{ justfile_directory() }}/tools/argument-comment-lint/run.py {args}

# Tail logs from the state SQLite database
[unix]
log *args:
    if [ "${1:-}" = "--" ]; then shift; fi; CARGO_GUARD_RESOURCE_PROFILE="${CARGO_GUARD_RESOURCE_PROFILE:-build}" bash ../scripts/cargo-guard.sh cargo run -p codex-cli --bin logs_client -- "$@"

[windows]
log *args:
    $forwarded_args = @($args | Select-Object -Skip 1); if ($forwarded_args.Count -gt 0 -and $forwarded_args[0] -eq "--") { $forwarded_args = @($forwarded_args | Select-Object -Skip 1) }; cargo run -p codex-cli --bin logs_client -- @forwarded_args
