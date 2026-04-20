#!/usr/bin/env bash
set -euo pipefail

# Merge-safety anchor: all workspace Cargo validation must go through this wrapper so Cargo-derived
# target/build resolution, the free-space floor, and the low-space-only cargo-clean policy stay deterministic.

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
CODEX_RS_DIR="${REPO_ROOT}/codex-rs"
CALLER_CWD="$(pwd)"
MIN_FREE_GIB="${CARGO_GUARD_MIN_FREE_GIB:-5}"

log() {
    local level="$1"
    shift
    printf '[cargo-guard][%s] %s\n' "${level}" "$*"
}

usage() {
    cat <<'EOF_HELP'
Usage:
  ./scripts/cargo-guard.sh <cargo-subcommand> [args...]
  ./scripts/cargo-guard.sh cargo <cargo-subcommand> [args...]

Runs Cargo with a deterministic free-space guardrail for build-like commands:
  - runs from ./codex-rs by default
  - preserves the caller cwd when `--manifest-path` is supplied so Cargo resolves that manifest/config context truthfully
  - derives the effective `target_directory` and `build_directory` from `cargo metadata`
  - requires at least 5 GiB free before starting a guarded build-like command
  - runs `cargo clean` only when the lowest free-space filesystem across the derived target/build dirs is below the floor
  - never runs `cargo clean` solely because the guarded Cargo command failed or was interrupted

Guarded build-like subcommands:
  - bench, build, check, clippy, doc, fix, install, nextest, run, rustc, test

Supported Cargo context inputs:
  - `+toolchain`
  - `--config <KEY=VALUE|PATH>` / `--config=<KEY=VALUE|PATH>`
  - `--manifest-path <path>` / `--manifest-path=<path>`
  - `--lockfile-path <path>` / `--lockfile-path=<path>`
  - `--target-dir <path>` / `--target-dir=<path>`
EOF_HELP
}

resolve_path() {
    local raw_path="$1"
    local base_dir="$2"
    if [[ "${raw_path}" = /* ]]; then
        realpath -m -- "${raw_path}"
    else
        realpath -m -- "${base_dir}/${raw_path}"
    fi
}

available_gib() {
    local path="$1"
    mkdir -p -- "${path}"
    local available
    available="$(df -BG --output=avail "${path}" | tail -n1 | tr -dc '0-9')"
    if [[ -z "${available}" ]]; then
        log error "failed to read free space for ${path}"
        exit 1
    fi
    printf '%s\n' "${available}"
}

metadata_field() {
    local json="$1"
    local field="$2"
    if command -v jq >/dev/null 2>&1; then
        printf '%s' "${json}" | jq -r --arg field "${field}" '.[$field] // empty'
        return
    fi

    printf '%s' "${json}" | tr -d '\n' | sed -nE "s/.*\"${field}\":\"([^\"]*)\".*/\1/p"
}

append_unique_path() {
    local candidate="$1"
    if [[ -z "${candidate}" ]]; then
        return
    fi
    local existing
    for existing in "${guard_paths[@]}"; do
        if [[ "${existing}" == "${candidate}" ]]; then
            return
        fi
    done
    guard_paths+=("${candidate}")
}

measure_guard_paths() {
    lowest_guard_gib=""
    lowest_guard_path=""
    local guard_path free_gib
    for guard_path in "${guard_paths[@]}"; do
        free_gib="$(available_gib "${guard_path}")"
        log info "guard-path: ${guard_path} (${free_gib} GiB free)"
        if [[ -z "${lowest_guard_gib}" ]] || (( free_gib < lowest_guard_gib )); then
            lowest_guard_gib="${free_gib}"
            lowest_guard_path="${guard_path}"
        fi
    done
}

is_guarded_subcommand() {
    case "$1" in
        bench|build|check|clippy|doc|fix|install|nextest|run|rustc|test)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

run_cargo_clean() {
    local reason="$1"
    log warning "running cargo clean (${reason})"
    (
        cd -- "${cargo_workdir}"
        cargo "${cargo_prefix_args[@]}" clean "${clean_context_args[@]}" "${clean_scope_args[@]}"
    )
}

resolve_metadata_dirs() {
    local metadata_json
    local metadata_cmd=(cargo "${cargo_prefix_args[@]}" metadata --format-version=1 --no-deps --quiet "${metadata_context_args[@]}")

    if [[ -n "${resolved_explicit_target_dir}" ]]; then
        metadata_json="$(
            cd -- "${cargo_workdir}"
            env CARGO_TARGET_DIR="${resolved_explicit_target_dir}" "${metadata_cmd[@]}"
        )"
    else
        metadata_json="$(
            cd -- "${cargo_workdir}"
            "${metadata_cmd[@]}"
        )"
    fi

    resolved_target_dir="$(metadata_field "${metadata_json}" target_directory)"
    resolved_build_dir="$(metadata_field "${metadata_json}" build_directory)"

    if [[ -z "${resolved_target_dir}" ]]; then
        log error "cargo metadata did not return target_directory"
        exit 1
    fi
    if [[ -z "${resolved_build_dir}" ]]; then
        resolved_build_dir="${resolved_target_dir}"
    fi
}

cleanup_on_exit() {
    local status=$?
    trap - EXIT

    if (( cargo_started == 1 )) && (( guard_enabled == 1 )); then
        measure_guard_paths
        if (( lowest_guard_gib < MIN_FREE_GIB )); then
            run_cargo_clean "post-run free space ${lowest_guard_gib} GiB at ${lowest_guard_path} below floor ${MIN_FREE_GIB} GiB (command exited with status ${status})"
            measure_guard_paths
            if (( lowest_guard_gib < MIN_FREE_GIB )); then
                log error "free space is still below the ${MIN_FREE_GIB} GiB floor after cargo clean (${lowest_guard_gib} GiB at ${lowest_guard_path})"
                if (( status == 0 )); then
                    status=1
                fi
            fi
        elif (( status != 0 )); then
            log info "command exited with status ${status}, but lowest free space ${lowest_guard_gib} GiB at ${lowest_guard_path} is above the ${MIN_FREE_GIB} GiB floor; skipping cargo clean"
        else
            log info "post-run lowest free space ${lowest_guard_gib} GiB at ${lowest_guard_path} is above the ${MIN_FREE_GIB} GiB floor; skipping cargo clean"
        fi
    fi

    exit "${status}"
}

if (($# == 0)); then
    usage
    exit 2
fi

if [[ "$1" == "--help" || "$1" == "-h" ]]; then
    usage
    exit 0
fi

cargo_args=("$@")
if [[ "${cargo_args[0]}" == "cargo" ]]; then
    cargo_args=("${cargo_args[@]:1}")
fi

if ((${#cargo_args[@]} == 0)); then
    usage
    exit 2
fi

if [[ ! -d "${CODEX_RS_DIR}" ]]; then
    log error "expected codex-rs at ${CODEX_RS_DIR}"
    exit 1
fi

if ! [[ "${MIN_FREE_GIB}" =~ ^[0-9]+$ ]] || (( MIN_FREE_GIB <= 0 )); then
    log error "CARGO_GUARD_MIN_FREE_GIB must be a positive integer; got ${MIN_FREE_GIB}"
    exit 2
fi

cargo_prefix_args=()
metadata_context_args=()
clean_context_args=()
clean_scope_args=()
cargo_chdir_values=()
manifest_path_raw=""
explicit_target_dir_raw=""
cargo_subcommand=""

index=0
while (( index < ${#cargo_args[@]} )); do
    arg="${cargo_args[$index]}"
    case "${arg}" in
        +*)
            if [[ -z "${cargo_subcommand}" ]]; then
                cargo_prefix_args+=("${arg}")
            fi
            ;;
        --config)
            (( index + 1 < ${#cargo_args[@]} )) || {
                log error "--config requires a value"
                exit 2
            }
            ((index += 1))
            metadata_context_args+=("--config" "${cargo_args[$index]}")
            clean_context_args+=("--config" "${cargo_args[$index]}")
            ;;
        --config=*)
            metadata_context_args+=("${arg}")
            clean_context_args+=("${arg}")
            ;;
        --manifest-path)
            (( index + 1 < ${#cargo_args[@]} )) || {
                log error "--manifest-path requires a value"
                exit 2
            }
            ((index += 1))
            manifest_path_raw="${cargo_args[$index]}"
            ;;
        --manifest-path=*)
            manifest_path_raw="${arg#--manifest-path=}"
            ;;
        --lockfile-path)
            (( index + 1 < ${#cargo_args[@]} )) || {
                log error "--lockfile-path requires a value"
                exit 2
            }
            ((index += 1))
            metadata_context_args+=("--lockfile-path" "${cargo_args[$index]}")
            clean_context_args+=("--lockfile-path" "${cargo_args[$index]}")
            ;;
        --lockfile-path=*)
            metadata_context_args+=("${arg}")
            clean_context_args+=("${arg}")
            ;;
        --locked|--offline|--frozen)
            metadata_context_args+=("${arg}")
            clean_context_args+=("${arg}")
            ;;
        -Z)
            (( index + 1 < ${#cargo_args[@]} )) || {
                log error "-Z requires a value"
                exit 2
            }
            ((index += 1))
            if [[ -z "${cargo_subcommand}" ]]; then
                cargo_prefix_args+=("-Z" "${cargo_args[$index]}")
            fi
            ;;
        -Z=*)
            if [[ -z "${cargo_subcommand}" ]]; then
                cargo_prefix_args+=("${arg}")
            fi
            ;;
        -C)
            (( index + 1 < ${#cargo_args[@]} )) || {
                log error "-C requires a value"
                exit 2
            }
            ((index += 1))
            if [[ -z "${cargo_subcommand}" ]]; then
                cargo_prefix_args+=("-C" "${cargo_args[$index]}")
                cargo_chdir_values+=("${cargo_args[$index]}")
            fi
            ;;
        -C=*)
            if [[ -z "${cargo_subcommand}" ]]; then
                cargo_prefix_args+=("${arg}")
                cargo_chdir_values+=("${arg#-C=}")
            fi
            ;;
        --target-dir)
            (( index + 1 < ${#cargo_args[@]} )) || {
                log error "--target-dir requires a value"
                exit 2
            }
            ((index += 1))
            explicit_target_dir_raw="${cargo_args[$index]}"
            ;;
        --target-dir=*)
            explicit_target_dir_raw="${arg#--target-dir=}"
            ;;
        -p|--package|--target|--profile)
            (( index + 1 < ${#cargo_args[@]} )) || {
                log error "${arg} requires a value"
                exit 2
            }
            ((index += 1))
            clean_scope_args+=("${arg}" "${cargo_args[$index]}")
            ;;
        -p=*|--package=*|--target=*|--profile=*)
            clean_scope_args+=("${arg}")
            ;;
        --workspace|--release|--doc)
            clean_scope_args+=("${arg}")
            ;;
        --color)
            (( index + 1 < ${#cargo_args[@]} )) || {
                log error "--color requires a value"
                exit 2
            }
            ((index += 1))
            if [[ -z "${cargo_subcommand}" ]]; then
                cargo_prefix_args+=("--color" "${cargo_args[$index]}")
            fi
            ;;
        --color=*|-q|--quiet|-v|--verbose|-vv)
            if [[ -z "${cargo_subcommand}" ]]; then
                cargo_prefix_args+=("${arg}")
            fi
            ;;
        -* )
            if [[ -z "${cargo_subcommand}" ]]; then
                cargo_prefix_args+=("${arg}")
            fi
            ;;
        *)
            if [[ -z "${cargo_subcommand}" ]]; then
                cargo_subcommand="${arg}"
            fi
            ;;
    esac
    ((index += 1))
done

cargo_workdir="${CODEX_RS_DIR}"
if [[ -n "${manifest_path_raw}" ]]; then
    cargo_workdir="${CALLER_CWD}"
fi

cargo_path_base_dir="${cargo_workdir}"
for chdir_value in "${cargo_chdir_values[@]}"; do
    cargo_path_base_dir="$(resolve_path "${chdir_value}" "${cargo_path_base_dir}")"
done

if [[ -n "${manifest_path_raw}" ]]; then
    resolved_manifest_path="$(resolve_path "${manifest_path_raw}" "${cargo_path_base_dir}")"
    if [[ ! -f "${resolved_manifest_path}" ]]; then
        log error "--manifest-path does not exist: ${resolved_manifest_path}"
        exit 2
    fi
    case "${resolved_manifest_path}" in
        "${CODEX_RS_DIR}"/*|"${CODEX_RS_DIR}")
            ;;
        *)
            log error "--manifest-path must stay under ${CODEX_RS_DIR}; got ${resolved_manifest_path}"
            exit 2
            ;;
    esac
    metadata_context_args+=("--manifest-path" "${resolved_manifest_path}")
    clean_context_args+=("--manifest-path" "${resolved_manifest_path}")
fi

resolved_explicit_target_dir=""
if [[ -n "${explicit_target_dir_raw}" ]]; then
    resolved_explicit_target_dir="$(resolve_path "${explicit_target_dir_raw}" "${cargo_path_base_dir}")"
    clean_context_args+=("--target-dir" "${resolved_explicit_target_dir}")
fi

if [[ -z "${cargo_subcommand}" ]]; then
    log info "no cargo subcommand detected; forwarding command without guard"
    (
        cd -- "${cargo_workdir}"
        cargo "${cargo_args[@]}"
    )
    exit $?
fi

guard_enabled=0
if is_guarded_subcommand "${cargo_subcommand}"; then
    guard_enabled=1
fi

if (( guard_enabled == 0 )); then
    log info "subcommand ${cargo_subcommand} does not produce guarded build artifacts; forwarding without free-space guard"
    (
        cd -- "${cargo_workdir}"
        cargo "${cargo_args[@]}"
    )
    exit $?
fi

resolve_metadata_dirs

guard_paths=()
append_unique_path "${resolved_target_dir}"
append_unique_path "${resolved_build_dir}"

log info "workspace: ${CODEX_RS_DIR}"
log info "execution cwd: ${cargo_workdir}"
log info "target-dir: ${resolved_target_dir}"
log info "build-dir: ${resolved_build_dir}"
measure_guard_paths
log info "pre-run lowest free space: ${lowest_guard_gib} GiB at ${lowest_guard_path}"

if (( lowest_guard_gib < MIN_FREE_GIB )); then
    run_cargo_clean "pre-run free space ${lowest_guard_gib} GiB at ${lowest_guard_path} below floor ${MIN_FREE_GIB} GiB"
    measure_guard_paths
    log info "post-clean lowest free space: ${lowest_guard_gib} GiB at ${lowest_guard_path}"
    if (( lowest_guard_gib < MIN_FREE_GIB )); then
        log error "free space is still below the ${MIN_FREE_GIB} GiB floor after cargo clean"
        exit 1
    fi
fi

cargo_started=1
trap cleanup_on_exit EXIT

(
    cd -- "${cargo_workdir}"
    cargo "${cargo_args[@]}"
)
