#!/usr/bin/env bash
set -euo pipefail

# Merge-safety anchor: all workspace Cargo validation must go through this wrapper so the
# target-dir resolution, free-space floor, and conditional cargo-clean policy stay deterministic.

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
CODEX_RS_DIR="${REPO_ROOT}/codex-rs"
MIN_FREE_GIB="${CARGO_GUARD_MIN_FREE_GIB:-10}"

log() {
    local level="$1"
    shift
    printf '[cargo-guard][%s] %s\n' "${level}" "$*"
}

usage() {
    cat <<'EOF'
Usage:
  ./scripts/cargo-guard.sh <cargo-subcommand> [args...]
  ./scripts/cargo-guard.sh cargo <cargo-subcommand> [args...]

Runs Cargo from ./codex-rs with a deterministic free-space guardrail:
  - resolves the effective target-dir for the exact command
  - requires at least 10 GiB free before starting
  - runs `cargo clean` only when space is below the floor
  - reruns `cargo clean` after failed/interrupted runs, or after successful runs that still leave
    the target filesystem below the floor

Supported target-dir override inputs:
  - `--target-dir <path>` / `--target-dir=<path>`
  - inline `--config build.target-dir=...`
  - `CARGO_BUILD_TARGET_DIR`
  - `CARGO_TARGET_DIR`
  - `codex-rs/.cargo/config.toml`
  - `~/.cargo/config.toml`

Deliberately unsupported:
  - `--manifest-path`
  - file-based `--config <path>`
EOF
}

strip_outer_quotes() {
    local value="$1"
    if [[ "${value}" == \"*\" && "${value}" == *\" ]]; then
        value="${value:1:${#value}-2}"
    elif [[ "${value}" == \'*\' && "${value}" == *\' ]]; then
        value="${value:1:${#value}-2}"
    fi
    printf '%s\n' "${value}"
}

is_unsupported_file_based_config_arg() {
    local config_value="$1"
    if [[ "${config_value}" == build.target-dir=* ]]; then
        return 1
    fi
    if [[ -f "${config_value}" ]]; then
        return 0
    fi
    if [[ "${config_value}" != /* && -f "${CODEX_RS_DIR}/${config_value}" ]]; then
        return 0
    fi
    return 1
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

extract_build_target_dir_from_config() {
    local config_path="$1"
    awk '
        BEGIN {
            in_build = 0;
            target = "";
        }
        /^\[build\][[:space:]]*$/ {
            in_build = 1;
            next;
        }
        /^\[/ {
            in_build = 0;
        }
        in_build && match($0, /^[[:space:]]*target-dir[[:space:]]*=[[:space:]]*"([^"]+)"/, parts) {
            target = parts[1];
        }
        END {
            if (target != "") {
                print target;
            }
        }
    ' "${config_path}"
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

run_cargo_clean() {
    local reason="$1"
    log warning "running cargo clean (${reason})"
    (
        cd -- "${CODEX_RS_DIR}"
        cargo "${cargo_global_args[@]}" clean
    )
}

resolve_target_dir() {
    if [[ -n "${explicit_target_dir}" ]]; then
        resolve_path "${explicit_target_dir}" "${CODEX_RS_DIR}"
        return
    fi

    if [[ -n "${inline_config_target_dir}" ]]; then
        resolve_path "${inline_config_target_dir}" "${CODEX_RS_DIR}"
        return
    fi

    if [[ -n "${CARGO_BUILD_TARGET_DIR:-}" ]]; then
        resolve_path "${CARGO_BUILD_TARGET_DIR}" "${CODEX_RS_DIR}"
        return
    fi

    if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
        resolve_path "${CARGO_TARGET_DIR}" "${CODEX_RS_DIR}"
        return
    fi

    local repo_config_target=""
    if [[ -f "${CODEX_RS_DIR}/.cargo/config.toml" ]]; then
        repo_config_target="$(extract_build_target_dir_from_config "${CODEX_RS_DIR}/.cargo/config.toml")"
        if [[ -n "${repo_config_target}" ]]; then
            resolve_path "${repo_config_target}" "${CODEX_RS_DIR}/.cargo"
            return
        fi
    fi

    local user_config_target=""
    if [[ -f "${HOME}/.cargo/config.toml" ]]; then
        user_config_target="$(extract_build_target_dir_from_config "${HOME}/.cargo/config.toml")"
        if [[ -n "${user_config_target}" ]]; then
            resolve_path "${user_config_target}" "${HOME}/.cargo"
            return
        fi
    fi

    resolve_path "target" "${CODEX_RS_DIR}"
}

cleanup_on_exit() {
    local status=$?
    trap - EXIT

    if (( cargo_started == 1 )) && [[ "${cargo_subcommand}" != "clean" ]]; then
        if (( status != 0 )); then
            run_cargo_clean "command exited with status ${status}"
        else
            local post_run_free_gib
            post_run_free_gib="$(available_gib "${resolved_target_dir}")"
            if (( post_run_free_gib < MIN_FREE_GIB )); then
                run_cargo_clean "post-run free space ${post_run_free_gib} GiB below floor ${MIN_FREE_GIB} GiB"
            else
                log info "post-run free space ${post_run_free_gib} GiB >= floor ${MIN_FREE_GIB} GiB; skipping cargo clean"
            fi
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

explicit_target_dir=""
inline_config_target_dir=""
cargo_global_args=()
cargo_subcommand=""

index=0
while (( index < ${#cargo_args[@]} )); do
    arg="${cargo_args[$index]}"
    case "${arg}" in
        --target-dir=*)
            explicit_target_dir="$(strip_outer_quotes "${arg#--target-dir=}")"
            cargo_global_args+=("${arg}")
            ;;
        --target-dir)
            (( index + 1 < ${#cargo_args[@]} )) || {
                log error "--target-dir requires a value"
                exit 2
            }
            ((index += 1))
            explicit_target_dir="$(strip_outer_quotes "${cargo_args[$index]}")"
            cargo_global_args+=("--target-dir" "${cargo_args[$index]}")
            ;;
        --config=*)
            config_value="${arg#--config=}"
            config_value="$(strip_outer_quotes "${config_value}")"
            if [[ "${config_value}" == build.target-dir=* ]]; then
                inline_config_target_dir="$(strip_outer_quotes "${config_value#build.target-dir=}")"
                cargo_global_args+=("${arg}")
            elif is_unsupported_file_based_config_arg "${config_value}"; then
                log error "file-based --config overrides are unsupported; use inline build.target-dir=... or extend the wrapper"
                exit 2
            fi
            ;;
        --config)
            (( index + 1 < ${#cargo_args[@]} )) || {
                log error "--config requires a value"
                exit 2
            }
            ((index += 1))
            config_value="$(strip_outer_quotes "${cargo_args[$index]}")"
            if [[ "${config_value}" == build.target-dir=* ]]; then
                inline_config_target_dir="$(strip_outer_quotes "${config_value#build.target-dir=}")"
                cargo_global_args+=("--config" "${cargo_args[$index]}")
            elif is_unsupported_file_based_config_arg "${config_value}"; then
                log error "file-based --config overrides are unsupported; use inline build.target-dir=... or extend the wrapper"
                exit 2
            fi
            ;;
        --manifest-path|--manifest-path=*)
            log error "--manifest-path is unsupported in this workspace wrapper; run from ./codex-rs via package selection flags instead"
            exit 2
            ;;
        -*)
            ;;
        *)
            if [[ -z "${cargo_subcommand}" ]]; then
                cargo_subcommand="${arg}"
            fi
            ;;
    esac
    ((index += 1))
done

if [[ -z "${cargo_subcommand}" ]]; then
    log error "missing cargo subcommand"
    usage
    exit 2
fi

if ! [[ "${MIN_FREE_GIB}" =~ ^[0-9]+$ ]] || (( MIN_FREE_GIB <= 0 )); then
    log error "CARGO_GUARD_MIN_FREE_GIB must be a positive integer; got ${MIN_FREE_GIB}"
    exit 2
fi

resolved_target_dir="$(resolve_target_dir)"
pre_run_free_gib="$(available_gib "${resolved_target_dir}")"

log info "workspace: ${CODEX_RS_DIR}"
log info "target-dir: ${resolved_target_dir}"
log info "pre-run free space: ${pre_run_free_gib} GiB"

if (( pre_run_free_gib < MIN_FREE_GIB )) && [[ "${cargo_subcommand}" != "clean" ]]; then
    run_cargo_clean "pre-run free space ${pre_run_free_gib} GiB below floor ${MIN_FREE_GIB} GiB"
    pre_run_free_gib="$(available_gib "${resolved_target_dir}")"
    log info "post-clean free space: ${pre_run_free_gib} GiB"
    if (( pre_run_free_gib < MIN_FREE_GIB )); then
        log error "free space is still below the ${MIN_FREE_GIB} GiB floor after cargo clean"
        exit 1
    fi
fi

cargo_started=1
trap cleanup_on_exit EXIT

(
    cd -- "${CODEX_RS_DIR}"
    cargo "${cargo_args[@]}"
)
