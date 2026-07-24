#!/usr/bin/env bash
set -euo pipefail

# Merge-safety anchor: all workspace build-like Cargo execution must stay behind this wrapper so
# disk-resource profiles, receipt-safe validation, target-dir discovery, and process-group cleanup stay centralized.

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
CODEX_RS_DIR="${REPO_ROOT}/codex-rs"
CALLER_CWD="$(pwd)"
BYTES_PER_GIB=1073741824
MIN_FREE_GIB="${CARGO_GUARD_MIN_FREE_GIB:-5}"
DEFAULT_CARGO_BUILD_JOBS=4
DEFAULT_TEST_RUST_MIN_STACK=8388608
DISK_EMERGENCY_STATUS=70
DEFAULT_HISTORY_PATH="${REPO_ROOT}/.sangoi/validation/history.jsonl"
TELEMETRY_SCHEMA_VERSION=1

cargo_started=0
guard_child_live=0
guard_child_pid=""
guard_child_pgid=""
monitor_pid=""
monitor_file=""
monitor_metrics_file=""
child_tmp_dir=""
metrics_started_at=0
metrics_start_available_bytes=()
metrics_min_available_bytes=()
metrics_end_available_bytes=()
telemetry_sample_seq=0
telemetry_error_count=0
telemetry_init_failed=0
telemetry_error_file=""

log() {
    local level="$1"
    shift
    printf '[cargo-guard][%s] %s\n' "${level}" "$*" >&2
}

usage() {
    cat <<'EOF_HELP'
Usage:
  ./scripts/cargo-guard.sh plan --changed --mode standard
  ./scripts/cargo-guard.sh plan --range BASE..HEAD --mode strict
  ./scripts/cargo-guard.sh plan --commit HEAD --mode standard
  ./scripts/cargo-guard.sh prep-plan --changed --mode standard
  ./scripts/cargo-guard.sh prep --changed --mode standard
  ./scripts/cargo-guard.sh verify --changed --mode standard
  ./scripts/cargo-guard.sh verify --range BASE..HEAD --mode strict
  ./scripts/cargo-guard.sh <cargo-subcommand> [args...]
  ./scripts/cargo-guard.sh cargo <cargo-subcommand> [args...]

Planner actions:
  - prep-plan: print the deterministic pre-review mechanical materialization plan
  - prep: execute the pre-review formatter/generator/lock materialization plan
  - plan: print the deterministic non-mutating cargo-validation plan
  - verify: execute the non-mutating validation plan and write validation receipts
  - verify collects every reachable command result by default; --fail-fast stops after the first
    command failure

Planner selectors:
  - --changed: tracked and untracked worktree changes
  - --commit <rev>: files changed by one non-merge commit
  - --range <rev-range>: files changed by a Git revision range, including deleted paths
  - --file <path>: explicit changed file
  - --surface <name>: explicit validation surface
  - --json: print machine-readable JSON plan output

Runs Cargo with deterministic guardrails for build-like commands:
  - runs from ./codex-rs by default
  - preserves the caller cwd when `--manifest-path` is supplied so Cargo resolves that manifest/config context truthfully
  - derives the effective `target_directory` and `build_directory` from `cargo metadata` under the same Cargo cwd/config context as the guarded build
  - selects guarded Cargo build parallelism from the active resource profile and live WSL/Linux CPU, memory, and disk signals
  - rejects Cargo build-job `-j/--jobs`, `--config build.jobs=...`, and nextest `--build-jobs` values above the selected build cap
  - treats `cargo nextest run -j/--jobs/--test-threads` as runtime test-thread controls capped by the active resource profile
  - rejects path-style `--config` and include-based `--config` job-cap bypasses
  - gives `cargo test` a default RUST_MIN_STACK=8388608 unless the caller already set RUST_MIN_STACK
  - uses byte-level disk measurements across workspace, target, build, tmp, and Cargo home filesystems
  - learns expected disk growth from .sangoi/validation/history.jsonl unless an explicit growth override is supplied
  - supervises every guarded Cargo child in a verified process group and aborts only that group on disk emergency
  - preserves successful package-targeted caches and runs package-scoped `cargo clean -p ...` only after package-targeted failures
  - runs broad `cargo clean` only when no package target is available, and never solely because a package-targeted command failed
  - also clears stale known target-cache contents when Cargo's effective target/build directory is elsewhere
  - honors CARGO_GUARD_NO_CLEAN=1 by failing before any pre-run or post-run cargo clean
  - honors CARGO_GUARD_NO_POST_CLEAN=1 by allowing pre-run cleanup but failing before post-run cleanup

Guarded build-like subcommands:
  - bench, build, check, clippy, doc, fix, install, nextest, run, rustc, test
EOF_HELP
}

require_command() {
    local command_name="$1"
    if ! command -v "${command_name}" >/dev/null 2>&1; then
        log error "required command not found: ${command_name}"
        exit 1
    fi
}

set_profile_default() {
    local env_name="$1"
    local env_value="$2"
    if [[ -z "${!env_name+x}" ]]; then
        export "${env_name}=${env_value}"
    fi
}

apply_resource_profile_defaults() {
    local profile_name="${CARGO_GUARD_RESOURCE_PROFILE:-}"
    if [[ -z "${profile_name}" ]]; then
        return
    fi

    require_command python3
    local config_path="${SCRIPT_DIR}/cargo-validation.toml"
    local profile_output
    if ! profile_output="$(
        python3 - "${config_path}" "${profile_name}" <<'PY'
import sys
import tomllib
from pathlib import Path

config_path = Path(sys.argv[1])
profile_name = sys.argv[2]
try:
    config = tomllib.loads(config_path.read_text())
except Exception as error:
    print(f"failed to read {config_path}: {error}", file=sys.stderr)
    raise SystemExit(2)

defaults = config.get("defaults", {})
for stale_key in (
    "history_growth_multiplier_pct",
    "success_history_growth_multiplier_pct",
    "disk_emergency_history_growth_multiplier_pct",
):
    if stale_key in defaults:
        print(f"defaults.{stale_key} uses a stale key name", file=sys.stderr)
        raise SystemExit(2)

profile = config.get("resource_profiles", {}).get(profile_name)
if not isinstance(profile, dict):
    print(f"resource profile {profile_name!r} is not defined in {config_path}", file=sys.stderr)
    raise SystemExit(2)

key_map = {
    "reserve_free_pct": "CARGO_GUARD_RESERVE_FREE_PCT",
    "reserve_free_gib": "CARGO_GUARD_RESERVE_FREE_GIB",
    "abort_free_pct": "CARGO_GUARD_ABORT_FREE_PCT",
    "abort_free_gib": "CARGO_GUARD_ABORT_FREE_GIB",
    "monitor_interval_secs": "CARGO_GUARD_MONITOR_INTERVAL_SECS",
    "test_threads": "CARGO_GUARD_TEST_THREADS_MAX",
    "low_disk_test_threads_max": "CARGO_GUARD_LOW_DISK_TEST_THREADS_MAX",
    "cargo_jobs_mode": "CARGO_GUARD_JOBS_MODE",
    "cargo_jobs_default": "CARGO_GUARD_JOBS_DEFAULT",
    "cargo_jobs_min": "CARGO_GUARD_JOBS_MIN",
    "cargo_jobs_max": "CARGO_GUARD_JOBS_MAX",
    "cargo_jobs_hard_max": "CARGO_GUARD_JOBS_HARD_MAX",
    "cargo_jobs_cpu_pct": "CARGO_GUARD_JOBS_CPU_PCT",
    "cargo_jobs_cpu_reserve": "CARGO_GUARD_JOBS_CPU_RESERVE",
    "cargo_jobs_mem_per_job_mib": "CARGO_GUARD_JOBS_MEM_PER_JOB_MIB",
    "cargo_jobs_mem_reserve_mib": "CARGO_GUARD_JOBS_MEM_RESERVE_MIB",
    "cargo_jobs_low_disk_max": "CARGO_GUARD_LOW_DISK_JOBS_MAX",
}
allowed_keys = set(key_map) | {"monitor"}
unknown_keys = sorted(set(profile) - allowed_keys)
if unknown_keys:
    print(f"resource profile {profile_name}.{unknown_keys[0]} is not supported", file=sys.stderr)
    raise SystemExit(2)

for profile_key, env_key in key_map.items():
    if profile_key not in profile:
        continue
    value = profile.get(profile_key)
    if profile_key == "cargo_jobs_mode":
        if value not in {"fixed", "auto"}:
            print(f"resource profile {profile_name}.{profile_key} must be fixed or auto", file=sys.stderr)
            raise SystemExit(2)
        print(f"{env_key}={value}")
        continue
    if profile_key == "cargo_jobs_default":
        if value not in {"min", "auto"}:
            print(f"resource profile {profile_name}.{profile_key} must be min or auto", file=sys.stderr)
            raise SystemExit(2)
        print(f"{env_key}={value}")
        continue
    if not isinstance(value, int) or isinstance(value, bool):
        print(f"resource profile {profile_name}.{profile_key} must be an integer", file=sys.stderr)
        raise SystemExit(2)
    print(f"{env_key}={value}")

monitor = profile.get("monitor")
if not isinstance(monitor, bool):
    print(f"resource profile {profile_name}.monitor must be boolean", file=sys.stderr)
    raise SystemExit(2)
print(f"CARGO_GUARD_MONITOR={1 if monitor else 0}")
PY
    )"; then
        log error "failed to load resource profile ${profile_name}"
        exit 2
    fi

    local line env_name env_value
    while IFS= read -r line; do
        [[ -n "${line}" ]] || continue
        env_name="${line%%=*}"
        env_value="${line#*=}"
        set_profile_default "${env_name}" "${env_value}"
    done <<<"${profile_output}"
}

require_nonnegative_int() {
    local value="$1"
    local name="$2"
    if ! [[ "${value}" =~ ^[0-9]+$ ]]; then
        log error "${name} must be a non-negative integer; got ${value}"
        exit 2
    fi
}

require_positive_int() {
    local value="$1"
    local name="$2"
    require_nonnegative_int "${value}" "${name}"
    if (( value <= 0 )); then
        log error "${name} must be a positive integer; got ${value}"
        exit 2
    fi
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

resolve_path_no_symlinks() {
    local raw_path="$1"
    local base_dir="$2"
    if [[ "${raw_path}" = /* ]]; then
        realpath -m -s -- "${raw_path}"
    else
        realpath -m -s -- "${base_dir}/${raw_path}"
    fi
}

bytes_from_gib() {
    local gib="$1"
    printf '%s\n' $((gib * BYTES_PER_GIB))
}

ceil_percent_bytes() {
    local total_bytes="$1"
    local pct="$2"
    printf '%s\n' $(((total_bytes * pct + 99) / 100))
}

max_bytes() {
    local max_value=0
    local value
    for value in "$@"; do
        if (( value > max_value )); then
            max_value="${value}"
        fi
    done
    printf '%s\n' "${max_value}"
}

compute_guard_command_fingerprint() {
    local profile_name="$1"
    local job_contract_digest="$2"
    shift 2
    python3 - "${profile_name}" "${job_contract_digest}" "$@" <<'PY'
import hashlib
import json
import sys

profile_name = sys.argv[1] or None
job_contract_digest = sys.argv[2]
argv = sys.argv[3:]
payload = json.dumps(
    {
        "schema": 2,
        "resource_profile": profile_name,
        "job_contract_digest": job_contract_digest,
        "argv": argv,
    },
    sort_keys=True,
    separators=(",", ":"),
)
print(hashlib.sha256(payload.encode("utf-8")).hexdigest())
PY
}

compute_job_contract_digest() {
    python3 - \
        "${CARGO_GUARD_RESOURCE_PROFILE:-}" \
        "${JOBS_MODE}" \
        "${JOBS_DEFAULT}" \
        "${JOBS_MIN}" \
        "${JOBS_MAX}" \
        "${JOBS_HARD_MAX}" \
        "${JOBS_LOW_DISK_MAX}" \
        "${JOBS_CPU_PCT}" \
        "${JOBS_CPU_RESERVE}" \
        "${JOBS_MEM_PER_JOB_MIB}" \
        "${JOBS_MEM_RESERVE_MIB}" <<'PY'
import hashlib
import json
import sys

payload = {
    "schema": 1,
    "resource_profile": sys.argv[1] or None,
    "jobs_mode": sys.argv[2],
    "jobs_default": sys.argv[3],
    "jobs_min": int(sys.argv[4]),
    "jobs_max": int(sys.argv[5]),
    "jobs_hard_max": int(sys.argv[6]),
    "jobs_low_disk_max": int(sys.argv[7]),
    "jobs_cpu_pct": int(sys.argv[8]),
    "jobs_cpu_reserve": int(sys.argv[9]),
    "jobs_mem_per_job_mib": int(sys.argv[10]),
    "jobs_mem_reserve_mib": int(sys.argv[11]),
}
print(
    hashlib.sha256(
        json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
)
PY
}

select_expected_growth_from_history() {
    local profile_name="$1"
    local fingerprint="$2"
    local history_path="$3"
    local config_path="$4"
    python3 - "${profile_name}" "${fingerprint}" "${history_path}" "${config_path}" <<'PY'
import json
import sys
import tomllib
from pathlib import Path

profile_name = sys.argv[1] or None
fingerprint = sys.argv[2]
history_path = Path(sys.argv[3])
config_path = Path(sys.argv[4])

try:
    config = tomllib.loads(config_path.read_text())
except Exception as error:
    print(f"failed to read {config_path}: {error}", file=sys.stderr)
    raise SystemExit(2)

defaults = config.get("defaults", {})
for stale_key in (
    "history_growth_multiplier_pct",
    "success_history_growth_multiplier_pct",
    "disk_emergency_history_growth_multiplier_pct",
):
    if stale_key in defaults:
        print(f"defaults.{stale_key} uses a stale key name", file=sys.stderr)
        raise SystemExit(2)

sample_limit = defaults.get("history_sample_limit", 20)
if not isinstance(sample_limit, int) or isinstance(sample_limit, bool) or sample_limit <= 0:
    print("defaults.history_sample_limit must be positive", file=sys.stderr)
    raise SystemExit(2)

matching_success: list[int] = []
matching_emergency: list[int] = []
if history_path.is_file():
    for raw_line in history_path.read_text(encoding="utf-8").splitlines():
        if not raw_line.strip():
            continue
        try:
            entry = json.loads(raw_line)
        except json.JSONDecodeError:
            continue
        if not isinstance(entry, dict):
            continue
        if entry.get("resource_profile") != profile_name:
            continue
        if entry.get("fingerprint") != fingerprint:
            continue
        observed = entry.get("observed_growth_gib")
        if not isinstance(observed, int) or isinstance(observed, bool) or observed < 0:
            continue
        risk_kind = entry.get("risk_kind")
        if risk_kind == "success" and entry.get("status") == 0 and entry.get("disk_emergency") is not True:
            matching_success.append(observed)
        elif risk_kind == "disk_emergency" and entry.get("disk_emergency") is True:
            matching_emergency.append(observed)

if not matching_success and not matching_emergency:
    print("0\tfallback:no-history")
    raise SystemExit(0)

candidates = [0]
source_parts: list[str] = []
if matching_success:
    sample = matching_success[-sample_limit:]
    history_growth = max(sample)
    candidates.append(history_growth)
    source_parts.append(f"success:max={history_growth},samples={len(sample)}")
if matching_emergency:
    sample = matching_emergency[-sample_limit:]
    history_growth = max(sample)
    candidates.append(history_growth)
    source_parts.append(f"disk_emergency:max={history_growth},samples={len(sample)}")
print(f"{max(candidates)}\thistory:{';'.join(source_parts)}")
PY
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

fs_id_for_path() {
    local path="$1"
    mkdir -p -- "${path}"
    stat -f -c %i -- "${path}"
}

df_size_avail_bytes() {
    local path="$1"
    mkdir -p -- "${path}"
    local output total available
    if ! output="$(df -B1 --output=size,avail "${path}" 2>/dev/null)"; then
        log error "failed to read byte-level free space for ${path}"
        exit 1
    fi
    read -r total available < <(printf '%s\n' "${output}" | awk 'NR == 2 { print $1, $2 }')
    if [[ -z "${total:-}" || -z "${available:-}" ]] || ! [[ "${total}" =~ ^[0-9]+$ && "${available}" =~ ^[0-9]+$ ]]; then
        log error "df did not return byte-level size/available data for ${path}"
        exit 1
    fi
    printf '%s %s\n' "${total}" "${available}"
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

append_unique_monitored_path() {
    local label="$1"
    local candidate="$2"
    if [[ -z "${candidate}" ]]; then
        return
    fi
    local existing
    for existing in "${monitored_paths[@]}"; do
        if [[ "${existing}" == "${candidate}" ]]; then
            return
        fi
    done
    monitored_labels+=("${label}")
    monitored_paths+=("${candidate}")
}

path_is_same_or_inside() {
    local candidate="$1"
    local parent="$2"
    [[ "${candidate}" == "${parent}" || "${candidate}" == "${parent}/"* ]]
}

append_stale_target_candidate() {
    local label="$1"
    local candidate="$2"
    local existing
    for existing in "${stale_target_candidate_paths[@]}"; do
        if [[ "${existing}" == "${candidate}" ]]; then
            return
        fi
    done
    stale_target_candidate_labels+=("${label}")
    stale_target_candidate_paths+=("${candidate}")
}

resolve_test_target_override() {
    local env_name="$1"
    local raw_path="$2"
    local target_kind="$3"
    local resolved_path tmp_root
    resolved_path="$(resolve_path_no_symlinks "${raw_path}" "${CODEX_RS_DIR}")"
    tmp_root="$(realpath -m -s -- "${TMPDIR:-/tmp}")"
    if ! path_is_same_or_inside "${resolved_path}" "${tmp_root}"; then
        log error "${env_name} must stay under ${tmp_root}; got ${resolved_path}"
        exit 2
    fi
    case "${target_kind}" in
        workspace)
            if [[ "${resolved_path}" != */target ]]; then
                log error "${env_name} must end with /target; got ${resolved_path}"
                exit 2
            fi
            ;;
        shared)
            if [[ "${resolved_path}" != */cargo-target/codex-rs ]]; then
                log error "${env_name} must end with /cargo-target/codex-rs; got ${resolved_path}"
                exit 2
            fi
            ;;
        *)
            log error "unknown stale target override kind: ${target_kind}"
            exit 2
            ;;
    esac
    printf '%s\n' "${resolved_path}"
}

resolve_stale_target_candidates() {
    stale_target_candidate_labels=()
    stale_target_candidate_paths=()

    local workspace_target_dir
    if [[ -n "${CARGO_GUARD_TEST_WORKSPACE_TARGET_DIR:-}" ]]; then
        workspace_target_dir="$(resolve_test_target_override CARGO_GUARD_TEST_WORKSPACE_TARGET_DIR "${CARGO_GUARD_TEST_WORKSPACE_TARGET_DIR}" workspace)"
    else
        workspace_target_dir="$(resolve_path_no_symlinks "${CODEX_RS_DIR}/target" "${CODEX_RS_DIR}")"
    fi
    append_stale_target_candidate "stale-target:workspace" "${workspace_target_dir}"

    local shared_target_dir
    if [[ -n "${CARGO_GUARD_TEST_SHARED_TARGET_DIR:-}" ]]; then
        shared_target_dir="$(resolve_test_target_override CARGO_GUARD_TEST_SHARED_TARGET_DIR "${CARGO_GUARD_TEST_SHARED_TARGET_DIR}" shared)"
    else
        shared_target_dir="$(resolve_path_no_symlinks "${HOME}/.cache/cargo-target/codex-rs" "${CODEX_RS_DIR}")"
    fi
    append_stale_target_candidate "stale-target:shared" "${shared_target_dir}"
}

stale_target_candidate_shape_is_allowed() {
    local label="$1"
    local path="$2"
    case "${label}" in
        stale-target:workspace)
            [[ "${path}" == */target ]]
            ;;
        stale-target:shared)
            [[ "${path}" == */cargo-target/codex-rs ]]
            ;;
        *)
            return 1
            ;;
    esac
}

stale_target_candidate_is_cleanable() {
    local label="$1"
    local path="$2"
    [[ -d "${path}" && ! -L "${path}" ]] || return 1

    local resolved_path candidate
    resolved_path="$(realpath -m -s -- "${path}")"
    stale_target_candidate_shape_is_allowed "${label}" "${resolved_path}" || return 1

    local known_candidate=0
    local index
    for index in "${!stale_target_candidate_paths[@]}"; do
        candidate="${stale_target_candidate_paths[$index]}"
        if [[ "${label}" == "${stale_target_candidate_labels[$index]}" && "${resolved_path}" == "${candidate}" ]]; then
            known_candidate=1
            break
        fi
    done
    (( known_candidate == 1 )) || return 1

    if path_is_same_or_inside "${resolved_path}" "${resolved_target_dir}" \
        || path_is_same_or_inside "${resolved_target_dir}" "${resolved_path}" \
        || path_is_same_or_inside "${resolved_path}" "${resolved_build_dir}" \
        || path_is_same_or_inside "${resolved_build_dir}" "${resolved_path}"; then
        return 1
    fi

    return 0
}

monitored_path_is_cleanable() {
    local label="$1"
    local path="$2"
    case "${label}" in
        target)
            return 0
            ;;
        build)
            path_is_same_or_inside "${path}" "${resolved_target_dir}"
            return $?
            ;;
        stale-target:*)
            stale_target_candidate_is_cleanable "${label}" "${path}"
            return $?
            ;;
        *)
            return 1
            ;;
    esac
}

record_config_build_jobs() {
    local config_value="$1"
    local compact_config normalized_config
    compact_config="$(printf '%s' "${config_value}" | tr -d '[:space:]')"
    normalized_config="${compact_config//\"/}"
    normalized_config="${normalized_config//\'/}"
    if [[ "${compact_config}" != *=* ]]; then
        explicit_config_jobs_error="path-style --config ${config_value} is rejected for guarded build-like commands because it may override build.jobs"
        return
    fi
    if [[ "${normalized_config}" =~ (^|[;,])include= ]]; then
        explicit_config_jobs_error="include-based --config ${config_value} is rejected for guarded build-like commands because it may override build.jobs"
        return
    fi
    local dotted_jobs_count=0
    local rest="${normalized_config}"
    while [[ "${rest}" == *"build.jobs="* ]]; do
        dotted_jobs_count=$((dotted_jobs_count + 1))
        rest="${rest#*build.jobs=}"
    done
    if (( dotted_jobs_count > 1 )); then
        explicit_config_jobs_error="Cargo build.jobs config was specified more than once"
        return
    fi
    if [[ "${normalized_config}" =~ build=\{([^}]*)\} ]]; then
        local inline_build_table="${BASH_REMATCH[1]}"
        local inline_jobs_count=0
        rest="${inline_build_table}"
        while [[ "${rest}" == *"jobs="* ]]; do
            inline_jobs_count=$((inline_jobs_count + 1))
            rest="${rest#*jobs=}"
        done
        if (( inline_jobs_count > 1 )); then
            explicit_config_jobs_error="Cargo build.jobs config was specified more than once"
            return
        fi
    fi
    if [[ "${normalized_config}" =~ (^|[;,])build\.jobs=([^;,]+)($|[;,]) ]]; then
        config_jobs_values+=("${BASH_REMATCH[2]}")
        return
    fi
    if [[ "${normalized_config}" =~ (^|[;,])build=\{[^}]*jobs=([^,}\;]+)[^}]*\}($|[;,]) ]]; then
        config_jobs_values+=("${BASH_REMATCH[2]}")
        return
    fi
    if [[ "${normalized_config}" == *"build.jobs"* ]] || [[ "${normalized_config}" == *"build={"*"jobs="* ]]; then
        explicit_config_jobs_error="unsupported Cargo build.jobs config form: ${config_value}"
    fi
}

normalize_positive_int() {
    local raw_value="$1"
    local label="$2"
    raw_value="${raw_value%\"}"
    raw_value="${raw_value#\"}"
    raw_value="${raw_value%\'}"
    raw_value="${raw_value#\'}"
    if ! [[ "${raw_value}" =~ ^[0-9]+$ ]] || (( raw_value <= 0 )); then
        log error "${label} must be a positive integer; got ${raw_value}"
        exit 2
    fi
    printf '%s\n' "${raw_value}"
}

read_cpu_count() {
    local cpu_count
    local nproc_cmd="${CARGO_GUARD_NPROC_CMD:-nproc}"
    if ! cpu_count="$(${nproc_cmd} 2>/dev/null)"; then
        log error "failed to read CPU count with ${nproc_cmd}"
        exit 2
    fi
    if ! [[ "${cpu_count}" =~ ^[0-9]+$ ]] || (( cpu_count <= 0 )); then
        log error "${nproc_cmd} must return a positive integer; got ${cpu_count}"
        exit 2
    fi
    printf '%s\n' "${cpu_count}"
}

read_mem_available_mib() {
    local meminfo_path="${CARGO_GUARD_MEMINFO_PATH:-/proc/meminfo}"
    local mem_kib
    if [[ ! -f "${meminfo_path}" ]]; then
        log error "meminfo path does not exist: ${meminfo_path}"
        exit 2
    fi
    mem_kib="$(awk '/^MemAvailable:/ { print $2; exit }' "${meminfo_path}")"
    if ! [[ "${mem_kib}" =~ ^[0-9]+$ ]]; then
        log error "MemAvailable must be present as an integer in ${meminfo_path}"
        exit 2
    fi
    printf '%s\n' $((mem_kib / 1024))
}

low_disk_jobs_active() {
    if (( EXPECTED_GROWTH_BYTES <= 0 )); then
        return 1
    fi
    local index available reserve slack
    for index in "${!monitored_paths[@]}"; do
        available="${monitored_available_bytes[$index]}"
        reserve="${monitored_reserve_bytes[$index]}"
        slack=$((available - reserve))
        if (( slack < 0 )); then
            slack=0
        fi
        if (( slack < EXPECTED_GROWTH_BYTES * 2 )); then
            return 0
        fi
    done
    return 1
}

select_cargo_build_jobs() {
    local mode="${JOBS_MODE}"
    local profile_cap="${JOBS_MAX}"
    local hard_cap="${JOBS_HARD_MAX}"
    if (( profile_cap > hard_cap )); then
        profile_cap="${hard_cap}"
    fi

    effective_test_threads_max="${TEST_THREADS_MAX}"
    low_disk_clamp=0
    if low_disk_jobs_active; then
        low_disk_clamp=1
        if (( JOBS_LOW_DISK_MAX < profile_cap )); then
            profile_cap="${JOBS_LOW_DISK_MAX}"
        fi
        if [[ -n "${LOW_DISK_TEST_THREADS_MAX}" ]]; then
            if [[ -z "${effective_test_threads_max}" || "${LOW_DISK_TEST_THREADS_MAX}" -lt "${effective_test_threads_max}" ]]; then
                effective_test_threads_max="${LOW_DISK_TEST_THREADS_MAX}"
            fi
        fi
    fi

    local selected_cap cpu_count cpu_scaled cpu_cap mem_available_mib usable_mem_mib memory_cap
    if [[ "${mode}" == "fixed" ]]; then
        selected_cap="${profile_cap}"
        cpu_count="n/a"
        cpu_cap="n/a"
        mem_available_mib="n/a"
        memory_cap="n/a"
    elif [[ "${mode}" == "auto" ]]; then
        cpu_count="$(read_cpu_count)"
        cpu_scaled=$((cpu_count * JOBS_CPU_PCT / 100))
        cpu_cap=$((cpu_scaled - JOBS_CPU_RESERVE))
        if (( cpu_cap < 1 )); then
            cpu_cap=1
        fi
        mem_available_mib="$(read_mem_available_mib)"
        usable_mem_mib=$((mem_available_mib - JOBS_MEM_RESERVE_MIB))
        if (( usable_mem_mib <= 0 )); then
            memory_cap=0
        else
            memory_cap=$((usable_mem_mib / JOBS_MEM_PER_JOB_MIB))
        fi
        selected_cap="${cpu_cap}"
        if (( memory_cap < selected_cap )); then
            selected_cap="${memory_cap}"
        fi
        if (( profile_cap < selected_cap )); then
            selected_cap="${profile_cap}"
        fi
        if (( hard_cap < selected_cap )); then
            selected_cap="${hard_cap}"
        fi
    else
        log error "CARGO_GUARD_JOBS_MODE must be fixed or auto; got ${mode}"
        exit 2
    fi

    selected_cargo_jobs_cap="${selected_cap}"
    if [[ "${JOBS_DEFAULT}" == "min" ]]; then
        selected_cargo_build_jobs="${JOBS_MIN}"
        selected_cargo_build_jobs_source="min"
    else
        selected_cargo_build_jobs="${selected_cap}"
        selected_cargo_build_jobs_source="auto"
    fi
    selected_cpu_count="${cpu_count}"
    selected_cpu_cap="${cpu_cap}"
    selected_mem_available_mib="${mem_available_mib}"
    selected_memory_cap="${memory_cap}"
    selected_nextest_test_threads=""
    selected_libtest_test_threads=""

    local explicit_build_job_sources
    explicit_build_job_sources=$((${#explicit_jobs_values[@]} + ${#config_jobs_values[@]}))
    if (( explicit_build_job_sources > 1 )); then
        log error "Cargo build-job count was specified more than once"
        exit 2
    fi

    if ((${#explicit_jobs_values[@]} == 1)); then
        selected_cargo_build_jobs="$(normalize_positive_int "${explicit_jobs_values[0]}" "explicit Cargo job count")"
        if (( selected_cargo_build_jobs > selected_cargo_jobs_cap )); then
            log error "explicit Cargo job count ${selected_cargo_build_jobs} exceeds selected cap ${selected_cargo_jobs_cap}"
            exit 2
        fi
        selected_cargo_build_jobs_source="explicit"
    fi
    if ((${#config_jobs_values[@]} > 0)); then
        local config_job_value
        config_job_value="$(normalize_positive_int "${config_jobs_values[0]}" "Cargo build.jobs config")"
        if (( config_job_value > selected_cargo_jobs_cap )); then
            log error "Cargo build.jobs config ${config_job_value} exceeds selected cap ${selected_cargo_jobs_cap}"
            exit 2
        fi
        selected_cargo_build_jobs="${config_job_value}"
        selected_cargo_build_jobs_source="config"
    fi
}

validate_selected_cargo_build_jobs() {
    if (( selected_cargo_build_jobs > selected_cargo_jobs_cap )); then
        if [[ "${selected_cargo_build_jobs_source}" == "min" ]]; then
            log error "adaptive Cargo jobs cap ${selected_cargo_jobs_cap} is below profile minimum ${JOBS_MIN}"
        else
            log error "selected Cargo job count ${selected_cargo_build_jobs} exceeds selected cap ${selected_cargo_jobs_cap}"
        fi
        exit 1
    fi
    if (( selected_cargo_jobs_cap < 1 )); then
        log error "selected Cargo job cap ${selected_cargo_jobs_cap} is below 1"
        exit 1
    fi
    if (( selected_cargo_build_jobs < 1 )); then
        log error "selected Cargo job count ${selected_cargo_build_jobs} is below 1"
        exit 1
    fi
}

resolve_metadata_dirs() {
    local metadata_json
    local metadata_cmd=(cargo "${cargo_prefix_args[@]}" metadata --format-version=1 --no-deps --quiet "${metadata_context_args[@]}")

    if [[ -n "${resolved_explicit_target_dir}" ]]; then
        metadata_json="$(
            cd -- "${cargo_workdir}"
            env -u CARGO_BUILD_JOBS -u RUST_TEST_THREADS -u NEXTEST_TEST_THREADS CARGO_TARGET_DIR="${resolved_explicit_target_dir}" "${metadata_cmd[@]}"
        )"
    else
        metadata_json="$(
            cd -- "${cargo_workdir}"
            env -u CARGO_BUILD_JOBS -u RUST_TEST_THREADS -u NEXTEST_TEST_THREADS "${metadata_cmd[@]}"
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

measure_monitored_paths() {
    monitored_total_bytes=()
    monitored_available_bytes=()
    monitored_fs_ids=()
    monitored_cleanable=()
    monitored_reserve_bytes=()
    monitored_required_bytes=()
    monitored_abort_bytes=()

    local index path label stats total available fs_id reserve_pct_bytes reserve_gib_bytes reserve_bytes required_bytes abort_pct_bytes abort_gib_bytes abort_bytes cleanable
    for index in "${!monitored_paths[@]}"; do
        path="${monitored_paths[$index]}"
        label="${monitored_labels[$index]}"
        stats="$(df_size_avail_bytes "${path}")"
        read -r total available <<<"${stats}"
        fs_id="$(fs_id_for_path "${path}")"
        reserve_pct_bytes="$(ceil_percent_bytes "${total}" "${RESERVE_FREE_PCT}")"
        reserve_gib_bytes="$(bytes_from_gib "${RESERVE_FREE_GIB}")"
        reserve_bytes="$(max_bytes "${reserve_pct_bytes}" "${reserve_gib_bytes}" "${MIN_FREE_BYTES}")"
        required_bytes=$((reserve_bytes + EXPECTED_GROWTH_BYTES))
        abort_pct_bytes="$(ceil_percent_bytes "${total}" "${ABORT_FREE_PCT}")"
        abort_gib_bytes="$(bytes_from_gib "${ABORT_FREE_GIB}")"
        abort_bytes="$(max_bytes "${abort_pct_bytes}" "${abort_gib_bytes}")"
        cleanable=0
        if monitored_path_is_cleanable "${label}" "${path}"; then
            cleanable=1
        fi
        monitored_total_bytes+=("${total}")
        monitored_available_bytes+=("${available}")
        monitored_fs_ids+=("${fs_id}")
        monitored_cleanable+=("${cleanable}")
        monitored_reserve_bytes+=("${reserve_bytes}")
        monitored_required_bytes+=("${required_bytes}")
        monitored_abort_bytes+=("${abort_bytes}")
        if ((${#metrics_min_available_bytes[@]} > index)); then
            if (( available < metrics_min_available_bytes[index] )); then
                metrics_min_available_bytes[$index]="${available}"
            fi
        fi
        log info "guard-path: ${label}=${path} avail=${available}B total=${total}B reserve=${reserve_bytes}B required-start=${required_bytes}B abort=${abort_bytes}B clean-candidate=${cleanable}"
    done
}

capture_start_metrics() {
    metrics_started_at="$(date +%s)"
    metrics_start_available_bytes=("${monitored_available_bytes[@]}")
    metrics_min_available_bytes=("${monitored_available_bytes[@]}")
    metrics_end_available_bytes=("${monitored_available_bytes[@]}")
}

capture_end_metrics() {
    metrics_end_available_bytes=("${monitored_available_bytes[@]}")
}

write_monitor_metrics_minima() {
    if [[ -z "${monitor_metrics_file}" ]]; then
        return
    fi
    local tmp_file="${monitor_metrics_file}.tmp.$$"
    : >"${tmp_file}"
    local index
    for index in "${!metrics_min_available_bytes[@]}"; do
        printf '%s\t%s\n' "${index}" "${metrics_min_available_bytes[$index]}" >>"${tmp_file}"
    done
    mv -- "${tmp_file}" "${monitor_metrics_file}"
}

merge_monitor_metrics_minima() {
    if [[ -z "${monitor_metrics_file}" || ! -f "${monitor_metrics_file}" ]]; then
        return
    fi
    local index available
    while IFS=$'\t' read -r index available; do
        if ! [[ "${index}" =~ ^[0-9]+$ && "${available}" =~ ^[0-9]+$ ]]; then
            log error "invalid monitor metrics sample in ${monitor_metrics_file}"
            exit 2
        fi
        if ((${#metrics_min_available_bytes[@]} > index)) && (( available < metrics_min_available_bytes[index] )); then
            metrics_min_available_bytes[$index]="${available}"
        fi
    done <"${monitor_metrics_file}"
}

telemetry_header() {
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        schema_version \
        row_type \
        sample_seq \
        unix_ms \
        elapsed_ms \
        phase \
        child_pid \
        child_pgid \
        jobs_selected \
        jobs_cap \
        jobs_default \
        jobs_source \
        mem_available_kib \
        swap_free_kib \
        loadavg_1 \
        loadavg_5 \
        loadavg_15 \
        psi_cpu_some_avg10 \
        psi_io_some_avg10 \
        psi_memory_some_avg10 \
        process_count_total \
        cargo_count \
        rustc_count \
        rustc_sum_rss_kib \
        rustc_max_rss_kib \
        linker_count \
        build_script_count \
        crate_name \
        process_comm \
        rss_kib \
        args_preview \
        sample_error
}

sync_file_durable() {
    local path="$1"
    python3 - "${path}" <<'PY'
import os
import sys

path = sys.argv[1]
fd = os.open(path, os.O_RDONLY)
try:
    if hasattr(os, "fdatasync"):
        os.fdatasync(fd)
    else:
        os.fsync(fd)
finally:
    os.close(fd)
PY
}

initialize_telemetry() {
    if [[ "${TELEMETRY_LEVEL}" == "off" ]]; then
        return
    fi
    local telemetry_dir
    telemetry_dir="$(dirname -- "${TELEMETRY_PATH}")"
    if ! mkdir -p -- "${telemetry_dir}"; then
        return 1
    fi
    if [[ ! -s "${TELEMETRY_PATH}" ]]; then
        if ! telemetry_header >"${TELEMETRY_PATH}"; then
            return 1
        fi
    fi
    if ! sync_file_durable "${TELEMETRY_PATH}"; then
        return 1
    fi
    if [[ "${TELEMETRY_LEVEL}" == "full" || "${TELEMETRY_LEVEL}" == "debug" ]]; then
        if ! collect_telemetry_sample initial; then
            return 1
        fi
    fi
}

collect_telemetry_sample() {
    if [[ "${TELEMETRY_LEVEL}" != "full" && "${TELEMETRY_LEVEL}" != "debug" ]]; then
        return 0
    fi
    telemetry_sample_seq=$((telemetry_sample_seq + 1))
    local phase="$1"
    local sampler_output
    if ! sampler_output="$(
        python3 - \
            "${TELEMETRY_PATH}" \
            "${TELEMETRY_LEVEL}" \
            "${TELEMETRY_SCHEMA_VERSION}" \
            "${telemetry_sample_seq}" \
            "${metrics_started_at}" \
            "${phase}" \
            "${guard_child_pid}" \
            "${guard_child_pgid}" \
            "${selected_cargo_build_jobs}" \
            "${selected_cargo_jobs_cap}" \
            "${JOBS_DEFAULT}" \
            "${selected_cargo_build_jobs_source}" <<'PY'
import csv
import os
import re
import subprocess
import sys
import time
from pathlib import Path

columns = [
    "schema_version",
    "row_type",
    "sample_seq",
    "unix_ms",
    "elapsed_ms",
    "phase",
    "child_pid",
    "child_pgid",
    "jobs_selected",
    "jobs_cap",
    "jobs_default",
    "jobs_source",
    "mem_available_kib",
    "swap_free_kib",
    "loadavg_1",
    "loadavg_5",
    "loadavg_15",
    "psi_cpu_some_avg10",
    "psi_io_some_avg10",
    "psi_memory_some_avg10",
    "process_count_total",
    "cargo_count",
    "rustc_count",
    "rustc_sum_rss_kib",
    "rustc_max_rss_kib",
    "linker_count",
    "build_script_count",
    "crate_name",
    "process_comm",
    "rss_kib",
    "args_preview",
    "sample_error",
]


def blank_row() -> dict[str, str]:
    return {column: "" for column in columns}


def safe_field(value: object, *, limit: int | None = None) -> str:
    text = str(value).replace("\t", " ").replace("\r", " ").replace("\n", " ")
    if limit is not None and len(text) > limit:
        return text[: max(0, limit - 3)] + "..."
    return text


def pressure_avg10(path: str) -> str:
    try:
        line = Path(path).read_text(encoding="utf-8").splitlines()[0]
    except (OSError, IndexError):
        return ""
    match = re.search(r"avg10=([0-9.]+)", line)
    return match.group(1) if match else ""


def meminfo() -> tuple[str, str]:
    values = {"MemAvailable": "", "SwapFree": ""}
    try:
        for line in Path("/proc/meminfo").read_text(encoding="utf-8").splitlines():
            key, _, rest = line.partition(":")
            if key in values:
                values[key] = rest.strip().split()[0]
    except OSError:
        pass
    return values["MemAvailable"], values["SwapFree"]


def crate_name_from_args(args: str) -> str:
    match = re.search(r"(?:^|\s)--crate-name(?:=|\s+)(\S+)", args)
    if match:
        return match.group(1)
    return ""


def args_preview(comm: str, args: str, crate_name: str) -> str:
    parts = [Path(comm).name or comm]
    if crate_name:
        parts.append(f"--crate-name {crate_name}")
    for flag in ("--edition", "--crate-type"):
        match = re.search(rf"(?:^|\s){re.escape(flag)}(?:=|\s+)(\S+)", args)
        if match:
            parts.append(f"{flag} {match.group(1)}")
    return safe_field(" ".join(parts), limit=240)


path = Path(sys.argv[1])
level = sys.argv[2]
schema_version = sys.argv[3]
sample_seq = sys.argv[4]
started_at = int(sys.argv[5] or "0")
phase = sys.argv[6]
child_pid = sys.argv[7]
child_pgid = sys.argv[8]
jobs_selected = sys.argv[9]
jobs_cap = sys.argv[10]
jobs_default = sys.argv[11]
jobs_source = sys.argv[12]
now = time.time()
base = blank_row()
base.update(
    {
        "schema_version": schema_version,
        "sample_seq": sample_seq,
        "unix_ms": str(int(now * 1000)),
        "elapsed_ms": str(max(0, int((now - started_at) * 1000))) if started_at else "0",
        "phase": safe_field(phase),
        "child_pid": safe_field(child_pid),
        "child_pgid": safe_field(child_pgid),
        "jobs_selected": safe_field(jobs_selected),
        "jobs_cap": safe_field(jobs_cap),
        "jobs_default": safe_field(jobs_default),
        "jobs_source": safe_field(jobs_source),
    }
)
rows: list[dict[str, str]] = []
detail_rows: list[dict[str, str]] = []
aggregate = dict(base)
aggregate.update({"row_type": "aggregate"})
try:
    mem_available, swap_free = meminfo()
    load_values = Path("/proc/loadavg").read_text(encoding="utf-8").split()[:3]
    ps_result = subprocess.run(
        ["ps", "-eo", "comm=,rss=,args="],
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if ps_result.returncode != 0:
        raise RuntimeError(f"ps failed: {ps_result.stderr.strip()}")

    process_count = 0
    cargo_count = 0
    rustc_processes: list[tuple[int, str, str, str]] = []
    linker_count = 0
    build_script_count = 0
    for raw_line in ps_result.stdout.splitlines():
        parts = raw_line.strip().split(None, 2)
        if len(parts) < 2:
            continue
        comm = parts[0]
        try:
            rss = int(parts[1])
        except ValueError:
            rss = 0
        args = parts[2] if len(parts) > 2 else ""
        process_count += 1
        lowered = f"{comm} {args}".lower()
        if "cargo" in lowered:
            cargo_count += 1
        if comm == "rustc" or " rustc" in f" {args}":
            rustc_processes.append((rss, comm, args, crate_name_from_args(args)))
        if comm in {"ld", "ld.lld", "mold"} or "ld.lld" in lowered or "mold" in lowered:
            linker_count += 1
        if "build-script" in lowered:
            build_script_count += 1

    rustc_sum = sum(process[0] for process in rustc_processes)
    rustc_max = max((process[0] for process in rustc_processes), default=0)
    aggregate.update(
        {
            "mem_available_kib": mem_available,
            "swap_free_kib": swap_free,
            "loadavg_1": load_values[0] if len(load_values) > 0 else "",
            "loadavg_5": load_values[1] if len(load_values) > 1 else "",
            "loadavg_15": load_values[2] if len(load_values) > 2 else "",
            "psi_cpu_some_avg10": pressure_avg10("/proc/pressure/cpu"),
            "psi_io_some_avg10": pressure_avg10("/proc/pressure/io"),
            "psi_memory_some_avg10": pressure_avg10("/proc/pressure/memory"),
            "process_count_total": str(process_count),
            "cargo_count": str(cargo_count),
            "rustc_count": str(len(rustc_processes)),
            "rustc_sum_rss_kib": str(rustc_sum),
            "rustc_max_rss_kib": str(rustc_max),
            "linker_count": str(linker_count),
            "build_script_count": str(build_script_count),
        }
    )

    if level == "debug":
        for rss, comm, args, crate_name in sorted(
            rustc_processes, reverse=True, key=lambda process: process[0]
        )[:5]:
            detail = dict(base)
            detail.update(
                {
                    "row_type": "detail",
                    "crate_name": safe_field(crate_name, limit=120),
                    "process_comm": safe_field(comm, limit=120),
                    "rss_kib": str(rss),
                    "args_preview": args_preview(comm, args, crate_name),
                }
            )
            detail_rows.append(detail)
except Exception as error:  # keep telemetry useful without killing the watchdog
    aggregate["sample_error"] = safe_field(error, limit=240)

rows.append(aggregate)
rows.extend(detail_rows)

with path.open("a", encoding="utf-8", newline="") as handle:
    writer = csv.DictWriter(
        handle,
        fieldnames=columns,
        delimiter="\t",
        lineterminator="\n",
        extrasaction="ignore",
    )
    for row in rows:
        writer.writerow(row)
    handle.flush()
    os.fsync(handle.fileno())
PY
    )"; then
        telemetry_error_count=$((telemetry_error_count + 1))
        if [[ -n "${telemetry_error_file}" ]]; then
            printf '1\n' >>"${telemetry_error_file}" 2>/dev/null || true
        fi
        log warning "telemetry sample failed: ${sampler_output}"
        return 1
    fi
    return 0
}

collect_failures() {
    local threshold_name="$1"
    failing_indexes=()
    local index threshold available
    for index in "${!monitored_paths[@]}"; do
        available="${monitored_available_bytes[$index]}"
        case "${threshold_name}" in
            required)
                threshold="${monitored_required_bytes[$index]}"
                ;;
            reserve)
                threshold="${monitored_reserve_bytes[$index]}"
                ;;
            abort)
                threshold="${monitored_abort_bytes[$index]}"
                ;;
            *)
                log error "unknown threshold ${threshold_name}"
                exit 2
                ;;
        esac
        if (( available < threshold )); then
            failing_indexes+=("${index}")
        fi
    done
}

failures_have_cleanable() {
    local index
    for index in "${failing_indexes[@]}"; do
        if (( monitored_cleanable[index] == 1 )); then
            return 0
        fi
    done
    return 1
}

log_failures() {
    local threshold_name="$1"
    local index path label available threshold cleanable
    for index in "${failing_indexes[@]}"; do
        path="${monitored_paths[$index]}"
        label="${monitored_labels[$index]}"
        available="${monitored_available_bytes[$index]}"
        cleanable="${monitored_cleanable[$index]}"
        case "${threshold_name}" in
            required) threshold="${monitored_required_bytes[$index]}" ;;
            reserve) threshold="${monitored_reserve_bytes[$index]}" ;;
            abort) threshold="${monitored_abort_bytes[$index]}" ;;
            *) threshold=0 ;;
        esac
        log warning "${threshold_name} free-space failure: ${label}=${path} available=${available}B threshold=${threshold}B clean-candidate=${cleanable}"
    done
}

write_failure_marker() {
    local marker_file="$1"
    : >"${marker_file}"
    local index
    for index in "${failing_indexes[@]}"; do
        printf '%s|%s|%s\n' "${monitored_paths[$index]}" "${monitored_cleanable[$index]}" "${monitored_available_bytes[$index]}" >>"${marker_file}"
    done
}

marker_has_cleanable() {
    local marker_file="$1"
    [[ -f "${marker_file}" ]] || return 1
    grep -Eq '\|1\|' "${marker_file}"
}

write_guard_metrics() {
    local status="$1"
    local disk_emergency="$2"
    if [[ -z "${GUARD_METRICS_PATH}" ]]; then
        return
    fi

    require_command python3
    local metrics_dir data_file tmp_path
    metrics_dir="$(dirname -- "${GUARD_METRICS_PATH}")"
    mkdir -p -- "${metrics_dir}"
    data_file="$(mktemp "${TMPDIR:-/tmp}/cargo-guard-metrics-data.XXXXXX")"
    tmp_path="${GUARD_METRICS_PATH}.tmp.$$"

    local index start_bytes min_bytes end_bytes
    for index in "${!monitored_paths[@]}"; do
        start_bytes="${metrics_start_available_bytes[$index]:-${monitored_available_bytes[$index]}}"
        min_bytes="${metrics_min_available_bytes[$index]:-${monitored_available_bytes[$index]}}"
        end_bytes="${metrics_end_available_bytes[$index]:-${monitored_available_bytes[$index]}}"
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
            "${monitored_labels[$index]}" \
            "${monitored_paths[$index]}" \
            "${monitored_fs_ids[$index]}" \
            "${monitored_total_bytes[$index]}" \
            "${start_bytes}" \
            "${min_bytes}" \
            "${end_bytes}" \
            "${monitored_cleanable[$index]}" >>"${data_file}"
    done

    python3 - "${data_file}" "${tmp_path}" "${cargo_subcommand}" "${resolved_target_dir}" "${resolved_build_dir}" "${selected_mem_available_mib}" <<'PY'
import json
import math
import os
import sys
import csv
from pathlib import Path

data_path = Path(sys.argv[1])
output_path = Path(sys.argv[2])
cargo_subcommand = sys.argv[3]
target_dir = sys.argv[4]
build_dir = sys.argv[5]
mem_available_raw = sys.argv[6]
monitored_paths = []
max_growth_bytes = 0
for raw_line in data_path.read_text(encoding="utf-8").splitlines():
    label, path, fs_id, _total, start, minimum, end, cleanable = raw_line.split("\t")
    start_i = int(start)
    minimum_i = int(minimum)
    end_i = int(end)
    growth = max(0, start_i - minimum_i)
    max_growth_bytes = max(max_growth_bytes, growth)
    monitored_paths.append(
        {
            "label": label,
            "path": path,
            "fs_id": fs_id,
            "cleanable": cleanable == "1",
            "start_available_bytes": start_i,
            "min_available_bytes": minimum_i,
            "end_available_bytes": end_i,
            "observed_growth_bytes": growth,
        }
    )

def parse_nonnegative_int(raw_value: str | None) -> int:
    if not raw_value:
        return 0
    return max(0, int(raw_value))


def read_telemetry_summary() -> dict[str, object]:
    telemetry_level = os.environ.get("CARGO_GUARD_TELEMETRY_LEVEL", "off")
    telemetry_path_raw = os.environ.get("CARGO_GUARD_TELEMETRY_PATH") or None
    telemetry_init_failed = os.environ.get("CARGO_GUARD_TELEMETRY_INIT_FAILED") == "1"
    summary: dict[str, object] = {
        "telemetry_level": telemetry_level,
        "telemetry_schema_version": int(os.environ.get("CARGO_GUARD_TELEMETRY_SCHEMA_VERSION", "1")),
        "telemetry_log_path": telemetry_path_raw,
        "telemetry_sample_count": 0,
        "telemetry_error_count": parse_nonnegative_int(
            os.environ.get("CARGO_GUARD_TELEMETRY_ERROR_COUNT")
        ),
        "top_rustc_crates": [],
    }
    if telemetry_level == "off" or telemetry_path_raw is None:
        summary["telemetry_log_path"] = None
        return summary
    if telemetry_init_failed:
        summary["telemetry_error_count"] = max(
            int(summary["telemetry_error_count"]), 1
        )
        return summary

    telemetry_path = Path(telemetry_path_raw)
    crate_stats: dict[str, dict[str, int]] = {}
    row_error_count = 0
    sample_count = 0
    try:
        with telemetry_path.open("r", encoding="utf-8", newline="") as telemetry_file:
            reader = csv.DictReader(telemetry_file, delimiter="\t")
            for row in reader:
                row_type = row.get("row_type", "")
                if row_type != "detail":
                    sample_count += 1
                if row.get("sample_error"):
                    row_error_count += 1
                crate_name = row.get("crate_name", "")
                if not crate_name:
                    continue
                rss_kib = parse_nonnegative_int(row.get("rss_kib"))
                stats = crate_stats.setdefault(
                    crate_name, {"samples": 0, "max_rss_kib": 0, "sum_rss_kib": 0}
                )
                stats["samples"] += 1
                stats["max_rss_kib"] = max(stats["max_rss_kib"], rss_kib)
                stats["sum_rss_kib"] += rss_kib
    except OSError:
        summary["telemetry_error_count"] = max(
            int(summary["telemetry_error_count"]), 1
        )
        return summary

    top_crates = [
        {"crate_name": crate_name, **stats}
        for crate_name, stats in sorted(
            crate_stats.items(),
            key=lambda item: (
                item[1]["max_rss_kib"],
                item[1]["sum_rss_kib"],
                item[1]["samples"],
                item[0],
            ),
            reverse=True,
        )[:10]
    ]
    summary["telemetry_sample_count"] = sample_count
    summary["telemetry_error_count"] = max(
        int(summary["telemetry_error_count"]), row_error_count
    )
    summary["top_rustc_crates"] = top_crates
    return summary


test_threads_raw = os.environ.get("CARGO_GUARD_METRICS_TEST_THREADS", "")
mem_available_selection_mib = None if mem_available_raw == "n/a" else int(mem_available_raw)
metrics = {
    "schema_version": 1,
    "resource_profile": os.environ.get("CARGO_GUARD_RESOURCE_PROFILE") or None,
    "command_fingerprint": os.environ["CARGO_GUARD_COMMAND_FINGERPRINT"],
    "job_contract_digest": os.environ["CARGO_GUARD_JOB_CONTRACT_DIGEST"],
    "cargo_subcommand": cargo_subcommand,
    "jobs_mode": os.environ.get("CARGO_GUARD_JOBS_MODE", "fixed"),
    "jobs_default": os.environ.get("CARGO_GUARD_METRICS_JOBS_DEFAULT", ""),
    "selected_cargo_build_job_cap": int(os.environ["CARGO_GUARD_SELECTED_JOBS_CAP"]),
    "effective_cargo_build_jobs": int(os.environ["CARGO_GUARD_SELECTED_JOBS"]),
    "effective_cargo_build_jobs_source": os.environ.get("CARGO_GUARD_METRICS_JOBS_SOURCE", ""),
    "selected_runtime_test_threads": int(test_threads_raw) if test_threads_raw else None,
    "target_dir": target_dir,
    "build_dir": build_dir,
    "monitored_paths": monitored_paths,
    "observed_growth_gib": int(math.ceil(max_growth_bytes / 1073741824)) if max_growth_bytes else 0,
    "mem_available_selection_mib": mem_available_selection_mib,
    "disk_emergency": os.environ["CARGO_GUARD_METRICS_DISK_EMERGENCY"] == "1",
    "status": int(os.environ["CARGO_GUARD_METRICS_STATUS"]),
}
metrics.update(read_telemetry_summary())
output_path.write_text(json.dumps(metrics, sort_keys=True) + "\n", encoding="utf-8")
PY
    mv -- "${tmp_path}" "${GUARD_METRICS_PATH}"
    rm -f -- "${data_file}"
}

append_direct_history_entry() {
    local status="$1"
    local disk_emergency="$2"
    if [[ -n "${GUARD_METRICS_PATH}" ]]; then
        return
    fi
    if (( status != 0 )) && [[ "${disk_emergency}" != "1" ]]; then
        return
    fi

    local history_path="${CARGO_GUARD_HISTORY_PATH:-${DEFAULT_HISTORY_PATH}}"
    local history_dir data_file
    history_dir="$(dirname -- "${history_path}")"
    mkdir -p -- "${history_dir}"
    data_file="$(mktemp "${TMPDIR:-/tmp}/cargo-guard-history-data.XXXXXX")"

    local index start_bytes min_bytes
    for index in "${!monitored_paths[@]}"; do
        start_bytes="${metrics_start_available_bytes[$index]:-${monitored_available_bytes[$index]}}"
        min_bytes="${metrics_min_available_bytes[$index]:-${monitored_available_bytes[$index]}}"
        printf '%s\t%s\n' "${start_bytes}" "${min_bytes}" >>"${data_file}"
    done

    local selected_runtime_threads="${selected_nextest_test_threads:-${selected_libtest_test_threads:-}}"
    python3 - \
        "${data_file}" \
        "${history_path}" \
        "${guard_command_fingerprint}" \
        "${CARGO_GUARD_RESOURCE_PROFILE:-}" \
        "${status}" \
        "${disk_emergency}" \
        "${selected_cargo_build_jobs}" \
        "${selected_cargo_build_jobs_source}" \
        "${JOBS_DEFAULT}" \
        "${job_contract_digest}" \
        "${selected_runtime_threads}" \
        "${metrics_started_at}" \
        -- "${canonical_guard_argv[@]}" <<'PY'
import json
import math
import sys
import time
from pathlib import Path

data_path = Path(sys.argv[1])
history_path = Path(sys.argv[2])
fingerprint = sys.argv[3]
resource_profile = sys.argv[4] or None
status = int(sys.argv[5])
disk_emergency = sys.argv[6] == "1"
selected_jobs = int(sys.argv[7])
selected_jobs_source = sys.argv[8]
jobs_default = sys.argv[9]
job_contract_digest = sys.argv[10]
test_threads_raw = sys.argv[11]
started_at = int(sys.argv[12])
separator_index = sys.argv.index("--")
argv = sys.argv[separator_index + 1 :]

max_growth_bytes = 0
for raw_line in data_path.read_text(encoding="utf-8").splitlines():
    start, minimum = raw_line.split("\t")
    growth = max(0, int(start) - int(minimum))
    max_growth_bytes = max(max_growth_bytes, growth)

recorded_at = time.time()
entry = {
    "argv": argv,
    "fingerprint": fingerprint,
    "resource_profile": resource_profile,
    "risk_kind": "disk_emergency" if disk_emergency else "success",
    "status": status,
    "disk_emergency": disk_emergency,
    "observed_growth_gib": int(math.ceil(max_growth_bytes / 1073741824)) if max_growth_bytes else 0,
    "selected_jobs": selected_jobs,
    "selected_jobs_source": selected_jobs_source,
    "jobs_default": jobs_default,
    "job_contract_digest": job_contract_digest,
    "test_threads": int(test_threads_raw) if test_threads_raw else None,
    "duration_seconds": round(recorded_at - started_at, 3),
    "recorded_at": recorded_at,
}

with history_path.open("a", encoding="utf-8") as handle:
    handle.write(json.dumps(entry, sort_keys=True) + "\n")
PY
    rm -f -- "${data_file}"
}

run_cargo_clean() {
    local reason="$1"
    shift
    local clean_args=("$@")
    if ((${#clean_args[@]} > 0)); then
        log warning "running targeted cargo clean: cargo clean ${clean_args[*]} (${reason})"
    else
        log warning "running cargo clean (${reason})"
    fi
    (
        cd -- "${cargo_workdir}"
        cargo "${cargo_prefix_args[@]}" clean "${clean_context_args[@]}" "${clean_args[@]}"
    )
}

has_package_clean_targets() {
    ((${#package_clean_args[@]} > 0))
}

clean_stale_target_caches() {
    local reason="$1"
    local index label path
    for index in "${!stale_target_candidate_paths[@]}"; do
        label="${stale_target_candidate_labels[$index]}"
        path="${stale_target_candidate_paths[$index]}"
        if ! stale_target_candidate_is_cleanable "${label}" "${path}"; then
            continue
        fi
        log warning "cleaning stale target cache: ${label}=${path} (${reason})"
        find "${path}" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +
    done
}

run_cargo_clean_or_fail() {
    local reason="$1"
    local phase="$2"
    shift 2
    local clean_args=("$@")
    if (( NO_CLEAN == 1 )); then
        log error "CARGO_GUARD_NO_CLEAN=1 forbids cargo clean (${reason})"
        return 1
    fi
    if [[ "${phase}" == "post" && "${NO_POST_CLEAN}" == "1" ]]; then
        log error "CARGO_GUARD_NO_POST_CLEAN=1 forbids cargo clean (${reason})"
        return 1
    fi
    if ! run_cargo_clean "${reason}" "${clean_args[@]}"; then
        return 1
    fi
    if ((${#clean_args[@]} == 0)); then
        clean_stale_target_caches "${reason}"
    fi
}

terminate_child_group() {
    local reason="$1"
    if (( guard_child_live == 0 )) || [[ -z "${guard_child_pgid}" ]]; then
        return
    fi
    if ! [[ "${guard_child_pgid}" =~ ^[0-9]+$ ]]; then
        log error "refusing to signal non-numeric child pgid: ${guard_child_pgid}"
        return
    fi
    log warning "terminating guarded Cargo process group ${guard_child_pgid} (${reason})"
    kill -TERM -- "-${guard_child_pgid}" 2>/dev/null || true
    local waited=0
    while kill -0 -- "-${guard_child_pgid}" 2>/dev/null && (( waited < TERM_GRACE_SECS )); do
        sleep 1
        waited=$((waited + 1))
    done
    if kill -0 -- "-${guard_child_pgid}" 2>/dev/null; then
        log warning "killing guarded Cargo process group ${guard_child_pgid} after ${TERM_GRACE_SECS}s grace"
        kill -KILL -- "-${guard_child_pgid}" 2>/dev/null || true
    fi
}

stop_monitor() {
    if [[ -n "${monitor_pid}" ]]; then
        kill "${monitor_pid}" 2>/dev/null || true
        wait "${monitor_pid}" 2>/dev/null || true
        monitor_pid=""
    fi
}

cleanup_process_group_on_exit() {
    local status=$?
    trap - EXIT
    if (( guard_child_live == 1 )); then
        terminate_child_group "cargo-guard exit"
        stop_monitor
        wait "${guard_child_pid}" 2>/dev/null || true
        guard_child_live=0
    fi
    if [[ -n "${child_tmp_dir}" ]]; then
        rm -rf -- "${child_tmp_dir}"
    fi
    exit "${status}"
}

handle_signal() {
    local signal_name="$1"
    local status=130
    if [[ "${signal_name}" == "TERM" ]]; then
        status=143
    fi
    trap - INT TERM
    terminate_child_group "received ${signal_name}"
    stop_monitor
    if (( guard_child_live == 1 )); then
        wait "${guard_child_pid}" 2>/dev/null || true
        guard_child_live=0
    fi
    exit "${status}"
}

start_disk_monitor() {
    (
        while kill -0 "${guard_child_pid}" 2>/dev/null; do
            sleep "${MONITOR_INTERVAL_SECS}"
            if ! kill -0 "${guard_child_pid}" 2>/dev/null; then
                break
            fi
            measure_monitored_paths
            write_monitor_metrics_minima
            collect_telemetry_sample periodic || true
            collect_failures abort
            if ((${#failing_indexes[@]} > 0)); then
                log error "free space fell below emergency abort threshold during guarded Cargo command"
                log_failures abort
                write_failure_marker "${monitor_file}"
                kill -TERM -- "-${guard_child_pgid}" 2>/dev/null || true
                sleep "${TERM_GRACE_SECS}"
                kill -KILL -- "-${guard_child_pgid}" 2>/dev/null || true
                exit 0
            fi
        done
    ) &
    monitor_pid="$!"
}

run_guarded_cargo() {
    child_tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/cargo-guard-child.XXXXXX")"
    local pid_file="${child_tmp_dir}/pid"
    local release_file="${child_tmp_dir}/release"
    monitor_file="${child_tmp_dir}/disk-emergency"
    monitor_metrics_file="${child_tmp_dir}/monitor-minima.tsv"
    telemetry_error_file="${child_tmp_dir}/telemetry-errors"
    local env_args=(CARGO_BUILD_JOBS="${resolved_cargo_build_jobs}")
    if [[ "${cargo_subcommand}" == "test" ]]; then
        env_args+=(RUST_MIN_STACK="${resolved_rust_min_stack}")
        if [[ -n "${selected_libtest_test_threads}" ]]; then
            env_args+=(RUST_TEST_THREADS="${selected_libtest_test_threads}")
        fi
    fi
    if [[ "${cargo_subcommand}" == "nextest" && -n "${selected_nextest_test_threads}" ]]; then
        env_args+=(NEXTEST_TEST_THREADS="${selected_nextest_test_threads}")
    fi

    (
        cd -- "${cargo_workdir}"
        setsid bash -c '
            set -euo pipefail
            pid_file="$1"
            release_file="$2"
            shift 2
            printf "%s\n" "$$" >"${pid_file}"
            while [[ ! -e "${release_file}" ]]; do
                sleep 0.02
            done
            exec "$@"
        ' _ "${pid_file}" "${release_file}" env "${env_args[@]}" cargo "${cargo_args[@]}"
    ) &
    guard_child_pid="$!"
    guard_child_live=1
    trap cleanup_process_group_on_exit EXIT
    trap 'handle_signal INT' INT
    trap 'handle_signal TERM' TERM

    local attempt child_pgid self_pgid
    for attempt in $(seq 1 200); do
        if [[ -s "${pid_file}" ]]; then
            break
        fi
        if ! kill -0 "${guard_child_pid}" 2>/dev/null; then
            log error "guarded Cargo launcher exited before writing pid"
            wait "${guard_child_pid}" 2>/dev/null || true
            guard_child_live=0
            return 1
        fi
        sleep 0.02
    done
    if [[ ! -s "${pid_file}" ]]; then
        log error "timed out waiting for guarded Cargo launcher pid"
        terminate_child_group "pid handshake timeout"
        wait "${guard_child_pid}" 2>/dev/null || true
        guard_child_live=0
        return 1
    fi

    guard_child_pid="$(cat "${pid_file}")"
    child_pgid="$(ps -o pgid= -p "${guard_child_pid}" | tr -d '[:space:]' || true)"
    self_pgid="$(ps -o pgid= -p $$ | tr -d '[:space:]' || true)"
    if ! [[ "${guard_child_pid}" =~ ^[0-9]+$ && "${child_pgid}" =~ ^[0-9]+$ ]] || [[ "${child_pgid}" != "${guard_child_pid}" ]] || [[ "${child_pgid}" == "${self_pgid}" ]]; then
        log error "failed to verify isolated child process group (pid=${guard_child_pid}, pgid=${child_pgid}, self-pgid=${self_pgid})"
        kill -TERM -- "${guard_child_pid}" 2>/dev/null || true
        sleep "${TERM_GRACE_SECS}"
        kill -KILL -- "${guard_child_pid}" 2>/dev/null || true
        wait "${guard_child_pid}" 2>/dev/null || true
        guard_child_live=0
        return 1
    fi
    guard_child_pgid="${child_pgid}"
    log info "guarded Cargo process group: pid=${guard_child_pid} pgid=${guard_child_pgid}"

    if ! initialize_telemetry; then
        telemetry_init_failed=1
        log error "failed to initialize Cargo guard telemetry"
        terminate_child_group "telemetry initialization failed before release"
        wait "${guard_child_pid}" 2>/dev/null || true
        guard_child_live=0
        return 1
    fi
    touch "${release_file}"
    if (( MONITOR_ENABLED == 1 )); then
        start_disk_monitor
    fi

    local status
    set +e
    wait "${guard_child_pid}"
    status=$?
    guard_child_live=0
    stop_monitor
    trap - INT TERM

    if [[ -f "${monitor_file}" && -s "${monitor_file}" ]]; then
        log error "guarded Cargo command ended after disk emergency"
        status="${DISK_EMERGENCY_STATUS}"
    fi
    return "${status}"
}

configure_nextest_args() {
    if [[ "${cargo_subcommand}" != "nextest" ]]; then
        return
    fi
    local nextest_index=-1
    local index
    for index in "${!cargo_args[@]}"; do
        if [[ "${cargo_args[$index]}" == "nextest" ]]; then
            nextest_index="${index}"
            break
        fi
    done
    if (( nextest_index < 0 )) || (( nextest_index + 1 >= ${#cargo_args[@]} )) || [[ "${cargo_args[$((nextest_index + 1))]}" != "run" ]]; then
        return
    fi

    local passthrough_index=-1
    for index in "${!cargo_args[@]}"; do
        if (( index < nextest_index + 2 )); then
            continue
        fi
        if [[ "${cargo_args[$index]}" == "--" ]]; then
            passthrough_index="${index}"
            break
        fi
    done

    local run_args_start=$((nextest_index + 2))
    local prefix_args=("${cargo_args[@]:0:nextest_index}")
    local pre_args=()
    local post_args=()
    if (( passthrough_index >= 0 )); then
        pre_args=("${cargo_args[@]:run_args_start:passthrough_index-run_args_start}")
        post_args=("${cargo_args[@]:passthrough_index}")
    else
        pre_args=("${cargo_args[@]:run_args_start}")
    fi

    local build_jobs_seen=0
    local runtime_seen=0
    local value arg
    index=0
    while (( index < ${#pre_args[@]} )); do
        arg="${pre_args[$index]}"
        case "${arg}" in
            --build-jobs)
                if (( build_jobs_seen == 1 )); then
                    log error "cargo nextest run build-job count was specified more than once"
                    exit 2
                fi
                if ((${#explicit_jobs_values[@]} + ${#config_jobs_values[@]} > 0)); then
                    log error "Cargo build-job count was specified more than once"
                    exit 2
                fi
                (( index + 1 < ${#pre_args[@]} )) || {
                    log error "cargo nextest run --build-jobs requires a positive integer value"
                    exit 2
                }
                value="${pre_args[$((index + 1))]}"
                value="$(normalize_positive_int "${value}" "cargo nextest --build-jobs")"
                if (( value > selected_cargo_jobs_cap )); then
                    log error "cargo nextest --build-jobs ${value} exceeds selected cap ${selected_cargo_jobs_cap}"
                    exit 2
                fi
                selected_cargo_build_jobs="${value}"
                selected_cargo_build_jobs_source="nextest-build-jobs"
                build_jobs_seen=1
                ((index += 1))
                ;;
            --build-jobs=*)
                if (( build_jobs_seen == 1 )); then
                    log error "cargo nextest run build-job count was specified more than once"
                    exit 2
                fi
                if ((${#explicit_jobs_values[@]} + ${#config_jobs_values[@]} > 0)); then
                    log error "Cargo build-job count was specified more than once"
                    exit 2
                fi
                value="$(normalize_positive_int "${arg#--build-jobs=}" "cargo nextest --build-jobs")"
                if (( value > selected_cargo_jobs_cap )); then
                    log error "cargo nextest --build-jobs ${value} exceeds selected cap ${selected_cargo_jobs_cap}"
                    exit 2
                fi
                selected_cargo_build_jobs="${value}"
                selected_cargo_build_jobs_source="nextest-build-jobs"
                build_jobs_seen=1
                ;;
            --test-threads|--jobs|-j)
                if (( runtime_seen == 1 )); then
                    log error "cargo nextest run runtime thread count was specified more than once"
                    exit 2
                fi
                (( index + 1 < ${#pre_args[@]} )) || {
                    log error "cargo nextest run ${arg} requires a positive integer runtime thread value"
                    exit 2
                }
                value="${pre_args[$((index + 1))]}"
                validate_nextest_runtime_threads "${value}" "${arg}"
                selected_nextest_test_threads="${value}"
                runtime_seen=1
                ((index += 1))
                ;;
            --test-threads=*|--jobs=*)
                if (( runtime_seen == 1 )); then
                    log error "cargo nextest run runtime thread count was specified more than once"
                    exit 2
                fi
                value="${arg#*=}"
                validate_nextest_runtime_threads "${value}" "${arg%%=*}"
                selected_nextest_test_threads="${value}"
                runtime_seen=1
                ;;
            -j*)
                if (( runtime_seen == 1 )); then
                    log error "cargo nextest run runtime thread count was specified more than once"
                    exit 2
                fi
                value="${arg#-j}"
                validate_nextest_runtime_threads "${value}" "-j"
                selected_nextest_test_threads="${value}"
                runtime_seen=1
                ;;
        esac
        ((index += 1))
    done

    if (( build_jobs_seen == 0 )); then
        pre_args+=("--build-jobs" "${selected_cargo_build_jobs}")
    fi
    if [[ -n "${effective_test_threads_max}" ]] && (( runtime_seen == 0 )); then
        pre_args+=("--test-threads" "${effective_test_threads_max}")
        selected_nextest_test_threads="${effective_test_threads_max}"
    fi
    cargo_args=("${prefix_args[@]}" "nextest" "run" "${pre_args[@]}" "${post_args[@]}")
}

validate_nextest_runtime_threads() {
    local value="$1"
    local label="$2"
    if [[ -z "${value}" || "${value}" == "--" ]]; then
        log error "cargo nextest run ${label} requires a positive integer runtime thread value"
        exit 2
    fi
    if ! [[ "${value}" =~ ^[0-9]+$ ]] || (( value <= 0 )); then
        if [[ -n "${effective_test_threads_max}" ]]; then
            log error "cargo nextest run ${label} must be a positive integer not greater than ${effective_test_threads_max}; got ${value}"
        else
            log error "cargo nextest run ${label} must be a positive integer; got ${value}"
        fi
        exit 2
    fi
    if [[ -z "${effective_test_threads_max}" ]]; then
        return
    fi
    if (( value > effective_test_threads_max )); then
        log error "cargo nextest runtime thread count ${value} exceeds cap ${effective_test_threads_max}"
        exit 2
    fi
}

configure_cargo_test_args() {
    if [[ "${cargo_subcommand}" != "test" || -z "${effective_test_threads_max}" ]]; then
        return
    fi

    local passthrough_index=-1
    local index
    for index in "${!cargo_args[@]}"; do
        if (( index < 1 )); then
            continue
        fi
        if [[ "${cargo_args[$index]}" == "--" ]]; then
            passthrough_index="${index}"
            break
        fi
    done

    local runtime_seen=0
    local value arg
    if (( passthrough_index >= 0 )); then
        index=$((passthrough_index + 1))
        while (( index < ${#cargo_args[@]} )); do
            arg="${cargo_args[$index]}"
            case "${arg}" in
                --test-threads)
                    if (( runtime_seen == 1 )); then
                        log error "cargo test runtime thread count was specified more than once"
                        exit 2
                    fi
                    (( index + 1 < ${#cargo_args[@]} )) || {
                        log error "cargo test --test-threads requires a positive integer runtime thread value"
                        exit 2
                    }
                    value="${cargo_args[$((index + 1))]}"
                    validate_libtest_runtime_threads "${value}" "--test-threads"
                    selected_libtest_test_threads="${value}"
                    runtime_seen=1
                    ((index += 1))
                    ;;
                --test-threads=*)
                    if (( runtime_seen == 1 )); then
                        log error "cargo test runtime thread count was specified more than once"
                        exit 2
                    fi
                    value="${arg#--test-threads=}"
                    validate_libtest_runtime_threads "${value}" "--test-threads"
                    selected_libtest_test_threads="${value}"
                    runtime_seen=1
                    ;;
            esac
            ((index += 1))
        done
    fi

    if (( runtime_seen == 0 )); then
        if (( passthrough_index >= 0 )); then
            cargo_args+=("--test-threads=${effective_test_threads_max}")
        else
            cargo_args+=("--" "--test-threads=${effective_test_threads_max}")
        fi
        selected_libtest_test_threads="${effective_test_threads_max}"
    fi
}

validate_libtest_runtime_threads() {
    local value="$1"
    local label="$2"
    if [[ -z "${value}" || "${value}" == "--" ]]; then
        log error "cargo test ${label} requires a positive integer runtime thread value"
        exit 2
    fi
    if ! [[ "${value}" =~ ^[0-9]+$ ]] || (( value <= 0 )); then
        log error "cargo test ${label} must be a positive integer not greater than ${effective_test_threads_max}; got ${value}"
        exit 2
    fi
    if (( value > effective_test_threads_max )); then
        log error "cargo test runtime thread count ${value} exceeds cap ${effective_test_threads_max}"
        exit 2
    fi
}

if (($# == 0)); then
    usage
    exit 2
fi

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    usage
    exit 0
fi

if [[ "${1:-}" == "plan" || "${1:-}" == "verify" || "${1:-}" == "prep-plan" || "${1:-}" == "prep" ]]; then
    exec python3 "${SCRIPT_DIR}/cargo-validate.py" "$@"
fi

if [[ ! -d "${CODEX_RS_DIR}" ]]; then
    log error "expected codex-rs at ${CODEX_RS_DIR}"
    exit 1
fi

require_command df
require_command stat
require_command setsid
require_command ps
require_command python3

apply_resource_profile_defaults

require_positive_int "${MIN_FREE_GIB}" "CARGO_GUARD_MIN_FREE_GIB"
MIN_FREE_BYTES="$(bytes_from_gib "${MIN_FREE_GIB}")"

RESERVE_FREE_PCT="${CARGO_GUARD_RESERVE_FREE_PCT:-0}"
RESERVE_FREE_GIB="${CARGO_GUARD_RESERVE_FREE_GIB:-${MIN_FREE_GIB}}"
EXPECTED_GROWTH_EXPLICIT=0
EXPECTED_GROWTH_SOURCE="fallback:no-history"
if [[ -n "${CARGO_GUARD_EXPECTED_GROWTH_GIB+x}" ]]; then
    EXPECTED_GROWTH_EXPLICIT=1
    EXPECTED_GROWTH_GIB="${CARGO_GUARD_EXPECTED_GROWTH_GIB}"
    EXPECTED_GROWTH_SOURCE="explicit-env"
else
    EXPECTED_GROWTH_GIB=0
fi
ABORT_FREE_PCT="${CARGO_GUARD_ABORT_FREE_PCT:-0}"
ABORT_FREE_GIB="${CARGO_GUARD_ABORT_FREE_GIB:-${MIN_FREE_GIB}}"
MONITOR_ENABLED="${CARGO_GUARD_MONITOR:-0}"
NO_CLEAN="${CARGO_GUARD_NO_CLEAN:-0}"
NO_POST_CLEAN="${CARGO_GUARD_NO_POST_CLEAN:-0}"
MONITOR_INTERVAL_SECS="${CARGO_GUARD_MONITOR_INTERVAL_SECS:-10}"
TERM_GRACE_SECS="${CARGO_GUARD_TERM_GRACE_SECS:-5}"
TEST_THREADS_MAX="${CARGO_GUARD_TEST_THREADS_MAX:-}"
LOW_DISK_TEST_THREADS_MAX="${CARGO_GUARD_LOW_DISK_TEST_THREADS_MAX:-}"
JOBS_MODE="${CARGO_GUARD_JOBS_MODE:-fixed}"
JOBS_DEFAULT="${CARGO_GUARD_JOBS_DEFAULT:-min}"
JOBS_MIN="${CARGO_GUARD_JOBS_MIN:-${DEFAULT_CARGO_BUILD_JOBS}}"
JOBS_MAX="${CARGO_GUARD_JOBS_MAX:-${DEFAULT_CARGO_BUILD_JOBS}}"
JOBS_HARD_MAX="${CARGO_GUARD_JOBS_HARD_MAX:-${JOBS_MAX}}"
JOBS_CPU_PCT="${CARGO_GUARD_JOBS_CPU_PCT:-100}"
JOBS_CPU_RESERVE="${CARGO_GUARD_JOBS_CPU_RESERVE:-0}"
JOBS_MEM_PER_JOB_MIB="${CARGO_GUARD_JOBS_MEM_PER_JOB_MIB:-1}"
JOBS_MEM_RESERVE_MIB="${CARGO_GUARD_JOBS_MEM_RESERVE_MIB:-0}"
JOBS_LOW_DISK_MAX="${CARGO_GUARD_LOW_DISK_JOBS_MAX:-${JOBS_MAX}}"
GUARD_METRICS_PATH="${CARGO_GUARD_METRICS_PATH:-}"
GUARD_COMMAND_FINGERPRINT="${CARGO_GUARD_COMMAND_FINGERPRINT:-}"
TELEMETRY_LEVEL="${CARGO_GUARD_TELEMETRY_LEVEL:-off}"
TELEMETRY_PATH="${CARGO_GUARD_TELEMETRY_PATH:-}"

require_nonnegative_int "${RESERVE_FREE_PCT}" "CARGO_GUARD_RESERVE_FREE_PCT"
require_positive_int "${RESERVE_FREE_GIB}" "CARGO_GUARD_RESERVE_FREE_GIB"
require_nonnegative_int "${EXPECTED_GROWTH_GIB}" "CARGO_GUARD_EXPECTED_GROWTH_GIB"
require_nonnegative_int "${ABORT_FREE_PCT}" "CARGO_GUARD_ABORT_FREE_PCT"
require_positive_int "${ABORT_FREE_GIB}" "CARGO_GUARD_ABORT_FREE_GIB"
require_positive_int "${MONITOR_INTERVAL_SECS}" "CARGO_GUARD_MONITOR_INTERVAL_SECS"
require_positive_int "${TERM_GRACE_SECS}" "CARGO_GUARD_TERM_GRACE_SECS"
if [[ "${MONITOR_ENABLED}" != "0" && "${MONITOR_ENABLED}" != "1" ]]; then
    log error "CARGO_GUARD_MONITOR must be 0 or 1; got ${MONITOR_ENABLED}"
    exit 2
fi
if [[ "${NO_CLEAN}" != "0" && "${NO_CLEAN}" != "1" ]]; then
    log error "CARGO_GUARD_NO_CLEAN must be 0 or 1; got ${NO_CLEAN}"
    exit 2
fi
if [[ "${NO_POST_CLEAN}" != "0" && "${NO_POST_CLEAN}" != "1" ]]; then
    log error "CARGO_GUARD_NO_POST_CLEAN must be 0 or 1; got ${NO_POST_CLEAN}"
    exit 2
fi
if [[ -n "${TEST_THREADS_MAX}" ]]; then
    require_positive_int "${TEST_THREADS_MAX}" "CARGO_GUARD_TEST_THREADS_MAX"
fi
if [[ -n "${LOW_DISK_TEST_THREADS_MAX}" ]]; then
    require_positive_int "${LOW_DISK_TEST_THREADS_MAX}" "CARGO_GUARD_LOW_DISK_TEST_THREADS_MAX"
fi
if [[ "${JOBS_MODE}" != "fixed" && "${JOBS_MODE}" != "auto" ]]; then
    log error "CARGO_GUARD_JOBS_MODE must be fixed or auto; got ${JOBS_MODE}"
    exit 2
fi
if [[ "${JOBS_DEFAULT}" != "min" && "${JOBS_DEFAULT}" != "auto" ]]; then
    log error "CARGO_GUARD_JOBS_DEFAULT must be min or auto; got ${JOBS_DEFAULT}"
    exit 2
fi
require_positive_int "${JOBS_MIN}" "CARGO_GUARD_JOBS_MIN"
require_positive_int "${JOBS_MAX}" "CARGO_GUARD_JOBS_MAX"
require_positive_int "${JOBS_HARD_MAX}" "CARGO_GUARD_JOBS_HARD_MAX"
require_positive_int "${JOBS_CPU_PCT}" "CARGO_GUARD_JOBS_CPU_PCT"
require_nonnegative_int "${JOBS_CPU_RESERVE}" "CARGO_GUARD_JOBS_CPU_RESERVE"
require_positive_int "${JOBS_MEM_PER_JOB_MIB}" "CARGO_GUARD_JOBS_MEM_PER_JOB_MIB"
require_nonnegative_int "${JOBS_MEM_RESERVE_MIB}" "CARGO_GUARD_JOBS_MEM_RESERVE_MIB"
require_positive_int "${JOBS_LOW_DISK_MAX}" "CARGO_GUARD_LOW_DISK_JOBS_MAX"
if (( JOBS_MIN > JOBS_MAX )); then
    log error "CARGO_GUARD_JOBS_MIN must not exceed CARGO_GUARD_JOBS_MAX"
    exit 2
fi
if (( JOBS_MAX > JOBS_HARD_MAX )); then
    log error "CARGO_GUARD_JOBS_MAX must not exceed CARGO_GUARD_JOBS_HARD_MAX"
    exit 2
fi
if (( JOBS_LOW_DISK_MAX > JOBS_MAX )); then
    log error "CARGO_GUARD_LOW_DISK_JOBS_MAX must not exceed CARGO_GUARD_JOBS_MAX"
    exit 2
fi
if [[ -n "${GUARD_METRICS_PATH}" && -z "${GUARD_COMMAND_FINGERPRINT}" ]]; then
    log error "CARGO_GUARD_METRICS_PATH requires CARGO_GUARD_COMMAND_FINGERPRINT"
    exit 2
fi
case "${TELEMETRY_LEVEL}" in
    off|summary|full|debug) ;;
    *)
        log error "CARGO_GUARD_TELEMETRY_LEVEL must be off, summary, full, or debug; got ${TELEMETRY_LEVEL}"
        exit 2
        ;;
esac
if [[ "${TELEMETRY_LEVEL}" != "off" && -z "${TELEMETRY_PATH}" ]]; then
    log error "CARGO_GUARD_TELEMETRY_LEVEL=${TELEMETRY_LEVEL} requires CARGO_GUARD_TELEMETRY_PATH"
    exit 2
fi

cargo_args=("$@")
if [[ "${cargo_args[0]}" == "cargo" ]]; then
    cargo_args=("${cargo_args[@]:1}")
fi

if ((${#cargo_args[@]} == 0)); then
    usage
    exit 2
fi
original_cargo_args=("${cargo_args[@]}")

cargo_prefix_args=()
metadata_context_args=()
clean_context_args=()
package_clean_args=()
cargo_chdir_values=()
manifest_path_raw=""
explicit_target_dir_raw=""
cargo_subcommand=""
explicit_jobs_values=()
explicit_config_jobs_error=""
config_jobs_values=()

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
            record_config_build_jobs "${cargo_args[$index]}"
            metadata_context_args+=("--config" "${cargo_args[$index]}")
            clean_context_args+=("--config" "${cargo_args[$index]}")
            ;;
        --config=*)
            record_config_build_jobs "${arg#--config=}"
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
        --)
            if [[ -n "${cargo_subcommand}" ]]; then
                break
            fi
            ;;
        -j|--jobs)
            if [[ "${cargo_subcommand}" == "nextest" ]]; then
                :
            else
                (( index + 1 < ${#cargo_args[@]} )) || {
                    log error "${arg} requires a value"
                    exit 2
                }
                ((index += 1))
                explicit_jobs_values+=("${cargo_args[$index]}")
            fi
            ;;
        -j*)
            if [[ "${arg}" != "-j" && "${cargo_subcommand}" != "nextest" ]]; then
                explicit_jobs_values+=("${arg#-j}")
            fi
            ;;
        --jobs=*)
            if [[ "${cargo_subcommand}" != "nextest" ]]; then
                explicit_jobs_values+=("${arg#--jobs=}")
            fi
            ;;
        -p|--package)
            (( index + 1 < ${#cargo_args[@]} )) || {
                log error "${arg} requires a value"
                exit 2
            }
            ((index += 1))
            package_clean_args+=("-p" "${cargo_args[$index]}")
            ;;
        --package=*)
            package_clean_args+=("-p" "${arg#--package=}")
            ;;
        -p*)
            package_clean_args+=("-p" "${arg#-p}")
            ;;
        --target|--profile)
            (( index + 1 < ${#cargo_args[@]} )) || {
                log error "${arg} requires a value"
                exit 2
            }
            ((index += 1))
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

if ! is_guarded_subcommand "${cargo_subcommand}"; then
    log info "subcommand ${cargo_subcommand} does not produce guarded build artifacts; forwarding without free-space guard"
    (
        cd -- "${cargo_workdir}"
        cargo "${cargo_args[@]}"
    )
    exit $?
fi

if [[ -n "${explicit_config_jobs_error}" ]]; then
    log error "${explicit_config_jobs_error}"
    exit 2
fi

canonical_guard_argv=("./scripts/cargo-guard.sh" "cargo" "${original_cargo_args[@]}")
job_contract_digest="$(compute_job_contract_digest)"
computed_guard_fingerprint="$(compute_guard_command_fingerprint "${CARGO_GUARD_RESOURCE_PROFILE:-}" "${job_contract_digest}" "${canonical_guard_argv[@]}")"
guard_command_fingerprint="${computed_guard_fingerprint}"
if [[ -n "${GUARD_COMMAND_FINGERPRINT}" ]]; then
    guard_command_fingerprint="${GUARD_COMMAND_FINGERPRINT}"
fi

if (( EXPECTED_GROWTH_EXPLICIT == 0 )); then
    history_path="${CARGO_GUARD_HISTORY_PATH:-${DEFAULT_HISTORY_PATH}}"
    growth_selection="$(
        select_expected_growth_from_history \
            "${CARGO_GUARD_RESOURCE_PROFILE:-}" \
            "${guard_command_fingerprint}" \
            "${history_path}" \
            "${SCRIPT_DIR}/cargo-validation.toml"
    )"
    IFS=$'\t' read -r EXPECTED_GROWTH_GIB EXPECTED_GROWTH_SOURCE <<<"${growth_selection}"
    require_nonnegative_int "${EXPECTED_GROWTH_GIB}" "learned expected growth"
    if [[ -z "${EXPECTED_GROWTH_SOURCE}" ]]; then
        log error "learned expected growth source is empty"
        exit 2
    fi
fi
EXPECTED_GROWTH_BYTES="$(bytes_from_gib "${EXPECTED_GROWTH_GIB}")"

resolve_metadata_dirs
resolve_stale_target_candidates

monitored_paths=()
monitored_labels=()
append_unique_monitored_path workspace "${CODEX_RS_DIR}"
append_unique_monitored_path target "${resolved_target_dir}"
append_unique_monitored_path build "${resolved_build_dir}"
for index in "${!stale_target_candidate_paths[@]}"; do
    candidate_path="${stale_target_candidate_paths[$index]}"
    if [[ -d "${candidate_path}" && ! -L "${candidate_path}" ]]; then
        append_unique_monitored_path "${stale_target_candidate_labels[$index]}" "${candidate_path}"
    fi
done
append_unique_monitored_path tmp "${TMPDIR:-/tmp}"
append_unique_monitored_path cargo-home "${CARGO_HOME:-${HOME}/.cargo}"

log info "workspace: ${CODEX_RS_DIR}"
log info "execution cwd: ${cargo_workdir}"
log info "target-dir: ${resolved_target_dir}"
log info "build-dir: ${resolved_build_dir}"
for index in "${!stale_target_candidate_paths[@]}"; do
    candidate_path="${stale_target_candidate_paths[$index]}"
    if [[ -d "${candidate_path}" && ! -L "${candidate_path}" ]]; then
        log info "${stale_target_candidate_labels[$index]}-dir: ${candidate_path}"
    fi
done
log info "resource-profile: ${CARGO_GUARD_RESOURCE_PROFILE:-manual}"
log info "disk-policy: min=${MIN_FREE_GIB}GiB reserve=${RESERVE_FREE_PCT}%/${RESERVE_FREE_GIB}GiB expected-growth=${EXPECTED_GROWTH_GIB}GiB source=${EXPECTED_GROWTH_SOURCE} abort=${ABORT_FREE_PCT}%/${ABORT_FREE_GIB}GiB monitor=${MONITOR_ENABLED}"

measure_monitored_paths
collect_failures required
if ((${#failing_indexes[@]} > 0)); then
    log_failures required
    if has_package_clean_targets; then
        log error "pre-run headroom is below threshold before a package-targeted command; preserving package caches instead of running broad cargo clean"
        exit 1
    elif failures_have_cleanable; then
        if ! run_cargo_clean_or_fail "pre-run cleanable filesystem below required start headroom" "pre"; then
            exit 1
        fi
        measure_monitored_paths
        collect_failures required
        if ((${#failing_indexes[@]} > 0)); then
            log_failures required
            log error "free space is still below required start headroom after cargo clean"
            exit 1
        fi
    else
        log error "required start headroom is unavailable only on non-cleanable monitored filesystems; cargo clean would not remediate this"
        exit 1
    fi
fi

select_cargo_build_jobs
configure_nextest_args
configure_cargo_test_args
validate_selected_cargo_build_jobs
resolved_cargo_build_jobs="${selected_cargo_build_jobs}"

log info "jobs-mode: ${JOBS_MODE}"
log info "cargo-build-jobs: selected=${selected_cargo_build_jobs} cap=${selected_cargo_jobs_cap} min=${JOBS_MIN} max=${JOBS_MAX} hard-max=${JOBS_HARD_MAX} default=${JOBS_DEFAULT} source=${selected_cargo_build_jobs_source} low-disk-clamp=${low_disk_clamp}"
log info "job-inputs: nproc=${selected_cpu_count} cpu-cap=${selected_cpu_cap} mem-available-mib=${selected_mem_available_mib} memory-cap=${selected_memory_cap}"
if [[ "${cargo_subcommand}" == "test" ]]; then
    resolved_rust_min_stack="${RUST_MIN_STACK:-${DEFAULT_TEST_RUST_MIN_STACK}}"
    log info "rust-min-stack: ${resolved_rust_min_stack} (cargo test)"
fi
if [[ "${cargo_subcommand}" == "nextest" && -n "${effective_test_threads_max}" ]]; then
    log info "nextest-test-threads-cap: ${effective_test_threads_max}"
fi
if [[ "${cargo_subcommand}" == "test" && -n "${effective_test_threads_max}" ]]; then
    log info "cargo-test-threads-cap: ${effective_test_threads_max}"
fi

capture_start_metrics
cargo_started=1
set +e
run_guarded_cargo
run_status=$?
set -e

measure_monitored_paths
merge_monitor_metrics_minima
capture_end_metrics
if [[ -f "${monitor_file}" && -s "${monitor_file}" ]]; then
    if marker_has_cleanable "${monitor_file}"; then
        if has_package_clean_targets; then
            if run_cargo_clean_or_fail "failed package artifacts after disk emergency" "post" "${package_clean_args[@]}"; then
                measure_monitored_paths
            fi
        elif run_cargo_clean_or_fail "disk emergency on target filesystem" "post"; then
            measure_monitored_paths
        fi
    else
        log error "disk emergency occurred on non-cleanable monitored filesystem; skipping cargo clean"
    fi
    collect_failures reserve
    if ((${#failing_indexes[@]} > 0)); then
        log_failures reserve
    fi
    run_status="${DISK_EMERGENCY_STATUS}"
else
    if (( run_status != 0 )) && has_package_clean_targets; then
        if run_cargo_clean_or_fail "failed package artifacts for nonzero rung" "post" "${package_clean_args[@]}"; then
            measure_monitored_paths
        fi
    fi
    collect_failures reserve
    if ((${#failing_indexes[@]} > 0)); then
        log_failures reserve
        if has_package_clean_targets; then
            log error "post-run reserve is below threshold after a package-targeted command; preserving unrelated package caches instead of running broad cargo clean"
            if (( run_status == 0 )); then
                run_status=1
            fi
        elif failures_have_cleanable; then
            if run_cargo_clean_or_fail "post-run cleanable filesystem below reserve" "post"; then
                measure_monitored_paths
                collect_failures reserve
                if ((${#failing_indexes[@]} > 0)); then
                    log_failures reserve
                    if (( run_status == 0 )); then
                        run_status=1
                    fi
                fi
            elif (( run_status == 0 )); then
                run_status=1
            fi
        else
            log error "post-run reserve is unavailable only on non-cleanable monitored filesystems; cargo clean would not remediate this"
            if (( run_status == 0 )); then
                run_status=1
            fi
        fi
    elif (( run_status != 0 )); then
        log info "command exited with status ${run_status}, but monitored free space is above reserve; skipping cargo clean"
    else
        log info "post-run monitored free space is above reserve; skipping cargo clean"
    fi
fi

capture_end_metrics
metrics_disk_emergency=0
if [[ "${run_status}" == "${DISK_EMERGENCY_STATUS}" ]]; then
    metrics_disk_emergency=1
fi
telemetry_metrics_error_count="${telemetry_error_count}"
if [[ -n "${telemetry_error_file}" && -f "${telemetry_error_file}" ]]; then
    telemetry_marker_error_count="$(wc -l <"${telemetry_error_file}" | tr -d '[:space:]')"
    if [[ "${telemetry_marker_error_count}" =~ ^[0-9]+$ && "${telemetry_marker_error_count}" -gt "${telemetry_metrics_error_count}" ]]; then
        telemetry_metrics_error_count="${telemetry_marker_error_count}"
    fi
fi
export CARGO_GUARD_COMMAND_FINGERPRINT="${guard_command_fingerprint}"
export CARGO_GUARD_JOB_CONTRACT_DIGEST="${job_contract_digest}"
export CARGO_GUARD_SELECTED_JOBS="${selected_cargo_build_jobs}"
export CARGO_GUARD_SELECTED_JOBS_CAP="${selected_cargo_jobs_cap}"
export CARGO_GUARD_METRICS_JOBS_DEFAULT="${JOBS_DEFAULT}"
export CARGO_GUARD_METRICS_JOBS_SOURCE="${selected_cargo_build_jobs_source}"
export CARGO_GUARD_METRICS_STATUS="${run_status}"
export CARGO_GUARD_METRICS_DISK_EMERGENCY="${metrics_disk_emergency}"
export CARGO_GUARD_METRICS_TEST_THREADS="${selected_nextest_test_threads:-${selected_libtest_test_threads:-}}"
export CARGO_GUARD_TELEMETRY_LEVEL="${TELEMETRY_LEVEL}"
export CARGO_GUARD_TELEMETRY_PATH="${TELEMETRY_PATH}"
export CARGO_GUARD_TELEMETRY_SCHEMA_VERSION="${TELEMETRY_SCHEMA_VERSION}"
export CARGO_GUARD_TELEMETRY_ERROR_COUNT="${telemetry_metrics_error_count}"
export CARGO_GUARD_TELEMETRY_INIT_FAILED="${telemetry_init_failed}"
write_guard_metrics "${run_status}" "${metrics_disk_emergency}"
append_direct_history_entry "${run_status}" "${metrics_disk_emergency}"

rm -rf -- "${child_tmp_dir}"
trap - EXIT
exit "${run_status}"
