#!/usr/bin/env bash
set -euo pipefail

# Merge-safety anchor: this harness protects the cargo-guard shell contract without invoking real Cargo.

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd -- "${REPO_ROOT}"

BYTES_PER_GIB=1073741824
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/cargo-guard-tests.XXXXXX")"
EXTRA_PIDS=()
cleanup() {
    local pid
    for pid in "${EXTRA_PIDS[@]:-}"; do
        kill -KILL "${pid}" 2>/dev/null || true
    done
    rm -rf -- "${TMP_ROOT}"
}
trap cleanup EXIT INT TERM HUP

FAKE_BIN="${TMP_ROOT}/bin"
mkdir -p -- "${FAKE_BIN}"

cat >"${FAKE_BIN}/cargo" <<'EOF_FAKE_CARGO'
#!/usr/bin/env bash
set -euo pipefail

log_file="${FAKE_CARGO_LOG:?FAKE_CARGO_LOG is required}"
{
    printf 'cargo|pwd=%s|args=' "$(pwd)"
    printf '%q ' "$@"
    printf '|jobs=%s|stack=%s|rust_threads=%s|nextest_threads=%s|target_env=%s\n' \
        "${CARGO_BUILD_JOBS:-unset}" \
        "${RUST_MIN_STACK:-unset}" \
        "${RUST_TEST_THREADS:-unset}" \
        "${NEXTEST_TEST_THREADS:-unset}" \
        "${CARGO_TARGET_DIR:-unset}"
} >>"${log_file}"

if [[ -n "${FAKE_CARGO_TELEMETRY_LINE_COUNT_FILE:-}" ]]; then
    mkdir -p -- "$(dirname -- "${FAKE_CARGO_TELEMETRY_LINE_COUNT_FILE}")"
    if [[ -n "${CARGO_GUARD_TELEMETRY_PATH:-}" && -f "${CARGO_GUARD_TELEMETRY_PATH}" ]]; then
        wc -l <"${CARGO_GUARD_TELEMETRY_PATH}" >"${FAKE_CARGO_TELEMETRY_LINE_COUNT_FILE}"
    else
        printf '0\n' >"${FAKE_CARGO_TELEMETRY_LINE_COUNT_FILE}"
    fi
fi

subcommand="${1:-}"
case "${subcommand}" in
    metadata)
        target_dir="${CARGO_TARGET_DIR:-${FAKE_CARGO_TARGET_DIR_JSON:?FAKE_CARGO_TARGET_DIR_JSON is required}}"
        build_dir="${CARGO_TARGET_DIR:-${FAKE_CARGO_BUILD_DIR_JSON:?FAKE_CARGO_BUILD_DIR_JSON is required}}"
        if [[ "${FAKE_CARGO_OMIT_BUILD_DIR_JSON:-0}" == "1" ]]; then
            printf '{"target_directory":"%s"}\n' "${target_dir}"
        else
            printf '{"target_directory":"%s","build_directory":"%s"}\n' \
                "${target_dir}" \
                "${build_dir}"
        fi
        ;;
    clean)
        exit "${FAKE_CARGO_CLEAN_STATUS:-0}"
        ;;
    *)
        if [[ -n "${FAKE_CARGO_DESCENDANT_FILE:-}" ]]; then
            (
                trap '' TERM
                while true; do
                    sleep 1
                done
            ) &
            printf '%s\n' "$!" >"${FAKE_CARGO_DESCENDANT_FILE}"
            while true; do
                sleep 1
            done
        fi
        if [[ "${FAKE_CARGO_BREAK_TELEMETRY_AFTER_START:-0}" == "1" && -n "${CARGO_GUARD_TELEMETRY_PATH:-}" ]]; then
            telemetry_parent="$(dirname -- "${CARGO_GUARD_TELEMETRY_PATH}")"
            rm -rf -- "${telemetry_parent}"
            : >"${telemetry_parent}"
        fi
        if [[ -n "${FAKE_CARGO_COMMAND_SLEEP:-}" ]]; then
            sleep "${FAKE_CARGO_COMMAND_SLEEP}"
        fi
        exit "${FAKE_CARGO_COMMAND_STATUS:-0}"
        ;;
esac
EOF_FAKE_CARGO

cat >"${FAKE_BIN}/df" <<'EOF_FAKE_DF'
#!/usr/bin/env bash
set -euo pipefail

bytes_per_gib=1073741824
value_gib="${FAKE_DF_DEFAULT_GIB:-99}"
if [[ -n "${FAKE_DF_SEQUENCE_FILE:-}" && -s "${FAKE_DF_SEQUENCE_FILE}" ]]; then
    value_gib="$(sed -n '1p' "${FAKE_DF_SEQUENCE_FILE}")"
    sed -n '2,$p' "${FAKE_DF_SEQUENCE_FILE}" >"${FAKE_DF_SEQUENCE_FILE}.next"
    mv -- "${FAKE_DF_SEQUENCE_FILE}.next" "${FAKE_DF_SEQUENCE_FILE}"
fi

total_gib="${FAKE_DF_TOTAL_GIB:-200}"
printf 'Size Avail\n%s %s\n' \
    "$((total_gib * bytes_per_gib))" \
    "$((value_gib * bytes_per_gib))"
EOF_FAKE_DF

cat >"${FAKE_BIN}/stat" <<'EOF_FAKE_STAT'
#!/usr/bin/env bash
set -euo pipefail

if [[ "$*" == *'-f -c %i'* ]]; then
    path="${@: -1}"
    if [[ -n "${FAKE_STAT_TARGET_PATH:-}" && "${path}" == "${FAKE_STAT_TARGET_PATH}" ]]; then
        printf '100\n'
    else
        printf '200\n'
    fi
    exit 0
fi

exec /usr/bin/stat "$@"
EOF_FAKE_STAT

cat >"${FAKE_BIN}/ps" <<'EOF_FAKE_PS'
#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${FAKE_PS_PGID_SEQUENCE_FILE:-}" && "$*" == *'pgid='* ]]; then
    value="$(sed -n '1p' "${FAKE_PS_PGID_SEQUENCE_FILE}")"
    sed -n '2,$p' "${FAKE_PS_PGID_SEQUENCE_FILE}" >"${FAKE_PS_PGID_SEQUENCE_FILE}.next"
    mv -- "${FAKE_PS_PGID_SEQUENCE_FILE}.next" "${FAKE_PS_PGID_SEQUENCE_FILE}"
    printf '%s\n' "${value}"
    exit 0
fi

if [[ "${FAKE_PS_FORCE_BAD_PGID:-0}" == "1" && "$*" == *'pgid='* ]]; then
    printf '99999\n'
    exit 0
fi

if [[ "${FAKE_PS_FAIL_PROCESS_LIST:-0}" == "1" && "$*" == *'comm=,rss=,args='* ]]; then
    printf 'forced process-list failure\n' >&2
    exit 3
fi

if [[ -n "${FAKE_PS_PROCESS_LIST_FILE:-}" && "$*" == *'comm=,rss=,args='* ]]; then
    cat "${FAKE_PS_PROCESS_LIST_FILE}"
    exit 0
fi

exec /usr/bin/ps "$@"
EOF_FAKE_PS

cat >"${FAKE_BIN}/nproc" <<'EOF_FAKE_NPROC'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "${FAKE_NPROC_VALUE:-28}"
EOF_FAKE_NPROC

chmod +x "${FAKE_BIN}/cargo" "${FAKE_BIN}/df" "${FAKE_BIN}/stat" "${FAKE_BIN}/ps" "${FAKE_BIN}/nproc"
export PATH="${FAKE_BIN}:${PATH}"

for fake_command in cargo df stat ps nproc; do
    if [[ "$(command -v "${fake_command}")" != "${FAKE_BIN}/${fake_command}" ]]; then
        echo "fake ${fake_command} is not first in PATH" >&2
        exit 1
    fi
done

TEST_INDEX=0
CURRENT_LOG=""
CURRENT_OUT=""
CURRENT_TARGET_DIR=""
CURRENT_BUILD_DIR=""
CURRENT_WORKSPACE_TARGET_DIR=""
CURRENT_SHARED_TARGET_DIR=""
CURRENT_SEQUENCE_FILE=""
CURRENT_MEMINFO=""
CURRENT_HISTORY=""

fail() {
    echo "[test-cargo-guard][FAIL] $*" >&2
    if [[ -n "${CURRENT_OUT}" && -f "${CURRENT_OUT}" ]]; then
        echo "--- command output ---" >&2
        cat "${CURRENT_OUT}" >&2
    fi
    if [[ -n "${CURRENT_LOG}" && -f "${CURRENT_LOG}" ]]; then
        echo "--- fake cargo log ---" >&2
        cat "${CURRENT_LOG}" >&2
    fi
    exit 1
}

begin_case() {
    TEST_INDEX=$((TEST_INDEX + 1))
    local name="case-${TEST_INDEX}"
    local case_dir="${TMP_ROOT}/${name}"
    mkdir -p -- "${case_dir}"
    CURRENT_LOG="${case_dir}/cargo.log"
    CURRENT_OUT="${case_dir}/out.log"
    CURRENT_TARGET_DIR="${case_dir}/target"
    CURRENT_BUILD_DIR="${CURRENT_TARGET_DIR}"
    CURRENT_WORKSPACE_TARGET_DIR="${case_dir}/codex-rs/target"
    CURRENT_SHARED_TARGET_DIR="${case_dir}/shared/cargo-target/codex-rs"
    CURRENT_SEQUENCE_FILE="${case_dir}/df-sequence"
    CURRENT_MEMINFO="${case_dir}/meminfo"
    CURRENT_HISTORY="${case_dir}/history.jsonl"
    : >"${CURRENT_LOG}"
    : >"${CURRENT_OUT}"
    : >"${CURRENT_SEQUENCE_FILE}"
    printf 'MemAvailable: %s kB\n' $((49152 * 1024)) >"${CURRENT_MEMINFO}"
    export FAKE_CARGO_LOG="${CURRENT_LOG}"
    export FAKE_CARGO_TARGET_DIR_JSON="${CURRENT_TARGET_DIR}"
    export FAKE_CARGO_BUILD_DIR_JSON="${CURRENT_BUILD_DIR}"
    export FAKE_CARGO_OMIT_BUILD_DIR_JSON=0
    export FAKE_DF_DEFAULT_GIB=99
    export FAKE_DF_TOTAL_GIB=200
    export FAKE_DF_SEQUENCE_FILE="${CURRENT_SEQUENCE_FILE}"
    export FAKE_STAT_TARGET_PATH="${CURRENT_TARGET_DIR}"
    export FAKE_NPROC_VALUE=28
    export CARGO_GUARD_MEMINFO_PATH="${CURRENT_MEMINFO}"
    export CARGO_GUARD_HISTORY_PATH="${CURRENT_HISTORY}"
    export CARGO_GUARD_TEST_WORKSPACE_TARGET_DIR="${CURRENT_WORKSPACE_TARGET_DIR}"
    export CARGO_GUARD_TEST_SHARED_TARGET_DIR="${CURRENT_SHARED_TARGET_DIR}"
    unset FAKE_CARGO_COMMAND_STATUS FAKE_CARGO_COMMAND_SLEEP FAKE_CARGO_CLEAN_STATUS FAKE_CARGO_TELEMETRY_LINE_COUNT_FILE FAKE_CARGO_BREAK_TELEMETRY_AFTER_START
    unset FAKE_PS_FORCE_BAD_PGID FAKE_PS_FAIL_PROCESS_LIST
    unset FAKE_CARGO_DESCENDANT_FILE FAKE_PS_PGID_SEQUENCE_FILE FAKE_PS_PROCESS_LIST_FILE
    unset RUST_MIN_STACK RUST_TEST_THREADS NEXTEST_TEST_THREADS CARGO_BUILD_JOBS CARGO_TARGET_DIR CARGO_GUARD_RESOURCE_PROFILE NEXTEST_PROFILE
    unset CARGO_GUARD_RESERVE_FREE_PCT CARGO_GUARD_RESERVE_FREE_GIB CARGO_GUARD_EXPECTED_GROWTH_GIB CARGO_GUARD_NO_CLEAN CARGO_GUARD_NO_POST_CLEAN
    unset CARGO_GUARD_ABORT_FREE_PCT CARGO_GUARD_ABORT_FREE_GIB CARGO_GUARD_MONITOR CARGO_GUARD_MONITOR_INTERVAL_SECS CARGO_GUARD_TERM_GRACE_SECS CARGO_GUARD_TEST_THREADS_MAX
    unset CARGO_GUARD_LOW_DISK_TEST_THREADS_MAX CARGO_GUARD_JOBS_MODE CARGO_GUARD_JOBS_DEFAULT CARGO_GUARD_JOBS_MIN CARGO_GUARD_JOBS_MAX CARGO_GUARD_JOBS_HARD_MAX
    unset CARGO_GUARD_JOBS_CPU_PCT CARGO_GUARD_JOBS_CPU_RESERVE CARGO_GUARD_JOBS_MEM_PER_JOB_MIB CARGO_GUARD_JOBS_MEM_RESERVE_MIB CARGO_GUARD_LOW_DISK_JOBS_MAX
    unset CARGO_GUARD_METRICS_PATH CARGO_GUARD_COMMAND_FINGERPRINT
    unset CARGO_GUARD_TELEMETRY_LEVEL CARGO_GUARD_TELEMETRY_PATH
    unset CARGO_GUARD_NPROC_CMD
}

set_df_sequence() {
    : >"${CURRENT_SEQUENCE_FILE}"
    local value
    for value in "$@"; do
        printf '%s\n' "${value}" >>"${CURRENT_SEQUENCE_FILE}"
    done
}

run_guard() {
    ./scripts/cargo-guard.sh "$@" >"${CURRENT_OUT}" 2>&1
}

run_command() {
    "$@" >"${CURRENT_OUT}" 2>&1
}

expect_ok() {
    if ! run_guard "$@"; then
        fail "expected success: ./scripts/cargo-guard.sh $*"
    fi
}

expect_command_ok() {
    if ! run_command "$@"; then
        fail "expected success: $*"
    fi
}

expect_fail() {
    if run_guard "$@"; then
        fail "expected failure: ./scripts/cargo-guard.sh $*"
    fi
}

assert_file_contains() {
    local file="$1"
    local pattern="$2"
    if ! grep -Eq -- "${pattern}" "${file}"; then
        fail "expected ${file} to contain pattern: ${pattern}"
    fi
}

assert_file_not_contains() {
    local file="$1"
    local pattern="$2"
    if grep -Eq -- "${pattern}" "${file}"; then
        fail "expected ${file} not to contain pattern: ${pattern}"
    fi
}

assert_file_empty() {
    local file="$1"
    if [[ -s "${file}" ]]; then
        fail "expected ${file} to be empty"
    fi
}

assert_clean_count() {
    local expected="$1"
    local actual
    actual="$(grep -Ec 'args=clean( |$)' "${CURRENT_LOG}" || true)"
    if [[ "${actual}" != "${expected}" ]]; then
        fail "expected ${expected} cargo clean calls, got ${actual}"
    fi
}

assert_package_clean_count() {
    local package="$1"
    local expected="$2"
    local actual
    actual="$(grep -Ec "args=clean -p ${package}( |$)" "${CURRENT_LOG}" || true)"
    if [[ "${actual}" != "${expected}" ]]; then
        fail "expected ${expected} cargo clean -p ${package} calls, got ${actual}"
    fi
}

assert_order() {
    local first="$1"
    local second="$2"
    local first_line second_line
    first_line="$(grep -En -- "${first}" "${CURRENT_LOG}" | head -n1 | cut -d: -f1 || true)"
    second_line="$(grep -En -- "${second}" "${CURRENT_LOG}" | head -n1 | cut -d: -f1 || true)"
    if [[ -z "${first_line}" || -z "${second_line}" || "${first_line}" -ge "${second_line}" ]]; then
        fail "expected pattern '${first}' before '${second}'"
    fi
}

write_history_entry() {
    local profile="$1"
    local growth="$2"
    local risk_kind="$3"
    local status="$4"
    shift 4
python3 - "${CURRENT_HISTORY}" "${profile}" "${growth}" "${risk_kind}" "${status}" "$@" <<'PY'
import hashlib
import json
import sys
import time
import tomllib
from pathlib import Path

history_path = Path(sys.argv[1])
profile = sys.argv[2] or None
growth = int(sys.argv[3])
risk_kind = sys.argv[4]
status = int(sys.argv[5])
argv = ["./scripts/cargo-guard.sh", "cargo", *sys.argv[6:]]
config = tomllib.loads(Path("scripts/cargo-validation.toml").read_text(encoding="utf-8"))
profile_config = config.get("resource_profiles", {}).get(profile, {}) if profile else {}
jobs_max = int(profile_config.get("cargo_jobs_max", 4))
job_contract_digest = hashlib.sha256(
    json.dumps(
        {
            "schema": 1,
            "resource_profile": profile,
            "jobs_mode": profile_config.get("cargo_jobs_mode", "fixed"),
            "jobs_default": profile_config.get("cargo_jobs_default", "min"),
            "jobs_min": int(profile_config.get("cargo_jobs_min", 4)),
            "jobs_max": jobs_max,
            "jobs_hard_max": int(profile_config.get("cargo_jobs_hard_max", jobs_max)),
            "jobs_low_disk_max": int(profile_config.get("cargo_jobs_low_disk_max", jobs_max)),
            "jobs_cpu_pct": int(profile_config.get("cargo_jobs_cpu_pct", 100)),
            "jobs_cpu_reserve": int(profile_config.get("cargo_jobs_cpu_reserve", 0)),
            "jobs_mem_per_job_mib": int(profile_config.get("cargo_jobs_mem_per_job_mib", 1)),
            "jobs_mem_reserve_mib": int(profile_config.get("cargo_jobs_mem_reserve_mib", 0)),
        },
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
).hexdigest()
fingerprint_payload = json.dumps(
    {
        "schema": 2,
        "resource_profile": profile,
        "job_contract_digest": job_contract_digest,
        "argv": argv,
    },
    sort_keys=True,
    separators=(",", ":"),
)
entry = {
    "argv": argv,
    "disk_emergency": risk_kind == "disk_emergency",
    "duration_seconds": 1.0,
    "fingerprint": hashlib.sha256(fingerprint_payload.encode("utf-8")).hexdigest(),
    "job_contract_digest": job_contract_digest,
    "jobs_default": profile_config.get("cargo_jobs_default", "min"),
    "observed_growth_gib": growth,
    "recorded_at": time.time(),
    "resource_profile": profile,
    "risk_kind": risk_kind,
    "selected_jobs": int(profile_config.get("cargo_jobs_min", 4)),
    "selected_jobs_source": "min",
    "status": status,
    "test_threads": None,
}
history_path.parent.mkdir(parents=True, exist_ok=True)
with history_path.open("a", encoding="utf-8") as handle:
    handle.write(json.dumps(entry, sort_keys=True) + "\n")
PY
}

wait_for_file() {
    local file="$1"
    local attempt
    for attempt in $(seq 1 100); do
        if [[ -s "${file}" ]]; then
            return
        fi
        sleep 0.05
    done
    fail "timed out waiting for ${file}"
}

begin_case
expect_ok --help
assert_file_contains "${CURRENT_OUT}" '--range BASE[.][.]HEAD'
assert_file_contains "${CURRENT_OUT}" '--commit <rev>'
assert_file_empty "${CURRENT_LOG}"

begin_case
expect_ok plan --help
assert_file_contains "${CURRENT_OUT}" 'usage: cargo-validate'
assert_file_contains "${CURRENT_OUT}" '--range'
assert_file_contains "${CURRENT_OUT}" '--commit'
assert_file_contains "${CURRENT_OUT}" '--json'
assert_file_empty "${CURRENT_LOG}"

begin_case
json_config="${TMP_ROOT}/json-plan-config.toml"
json_metadata="${TMP_ROOT}/json-plan-metadata.json"
cat >"${json_config}" <<'EOF_JSON_PLAN_CONFIG'
schema_version = 1

[commands.just-summary]
argv = ["just", "--summary"]

[[path_rules]]
patterns = ["justfile"]
commands = ["just-summary"]
EOF_JSON_PLAN_CONFIG
printf '{"packages":[]}\n' >"${json_metadata}"
expect_ok plan --file justfile --mode standard --json --no-receipt --config "${json_config}" --metadata-json "${json_metadata}"
python3 - "${CURRENT_OUT}" <<'PY'
import json
import sys
from pathlib import Path

payload = json.loads(Path(sys.argv[1]).read_text())
assert payload["action"] == "plan"
assert payload["mode"] == "standard"
assert payload["telemetry_level"] == "full"
assert payload["changed_files"] == ["justfile"]
assert any(command["argv"] == ["just", "--summary"] for command in payload["commands"])
PY
assert_file_empty "${CURRENT_LOG}"

begin_case
expect_ok check -p codex-core
assert_file_contains "${CURRENT_LOG}" 'args=metadata .*'
assert_file_contains "${CURRENT_LOG}" 'args=check -p codex-core '
assert_file_contains "${CURRENT_OUT}" 'guarded Cargo process group: pid=[0-9]+ pgid=[0-9]+'

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=workspace_nextest
export FAKE_DF_DEFAULT_GIB=190
expect_ok cargo nextest run --no-fail-fast
assert_file_contains "${CURRENT_OUT}" 'disk-policy: min=5GiB reserve=0%/5GiB expected-growth=0GiB source=fallback:no-history abort=0%/5GiB monitor=1'
assert_file_contains "${CURRENT_OUT}" 'jobs-mode: auto'
assert_file_contains "${CURRENT_OUT}" 'cargo-build-jobs: selected=4 cap=8 .*default=min source=min'
assert_file_contains "${CURRENT_LOG}" 'args=nextest run --no-fail-fast --build-jobs 4 --test-threads 1 '

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=workspace_nextest
export FAKE_DF_DEFAULT_GIB=40
expect_ok cargo nextest run
assert_clean_count 0
assert_file_contains "${CURRENT_OUT}" 'disk-policy: min=5GiB reserve=0%/5GiB expected-growth=0GiB source=fallback:no-history abort=0%/5GiB monitor=1'

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=workspace_nextest
export CARGO_GUARD_EXPECTED_GROWTH_GIB=7
export CARGO_GUARD_MONITOR=0
expect_ok cargo nextest run --test-threads 1
assert_file_contains "${CURRENT_OUT}" 'disk-policy: min=5GiB reserve=0%/5GiB expected-growth=7GiB source=explicit-env abort=0%/5GiB monitor=0'

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=workspace_nextest
write_history_entry workspace_nextest 18 success 0 nextest run
expect_ok cargo nextest run
assert_file_contains "${CURRENT_OUT}" 'disk-policy: min=5GiB reserve=0%/5GiB expected-growth=18GiB source=history:success:max=18,samples=1 abort=0%/5GiB monitor=1'

begin_case
export FAKE_DF_DEFAULT_GIB=190
expect_command_ok just test
assert_file_contains "${CURRENT_OUT}" 'disk-policy: min=5GiB reserve=0%/5GiB expected-growth=0GiB source=fallback:no-history abort=0%/5GiB monitor=1'
assert_file_contains "${CURRENT_LOG}" 'args=nextest run --profile local-safe --build-jobs 4 --test-threads 1 '

begin_case
export NEXTEST_PROFILE=local-disk-tight
export FAKE_DF_DEFAULT_GIB=190
expect_command_ok just test
unset NEXTEST_PROFILE
assert_file_contains "${CURRENT_OUT}" 'resource-profile: workspace_nextest_tight'
assert_file_contains "${CURRENT_LOG}" 'args=nextest run --profile local-disk-tight --build-jobs 3 --test-threads 1 '

begin_case
helper_log="$(dirname -- "${CURRENT_LOG}")/tui-with-exec-server.log"
mkdir -p -- "${CURRENT_TARGET_DIR}/debug"
cat >"${CURRENT_TARGET_DIR}/debug/codex" <<'EOF_FAKE_CODEX'
#!/usr/bin/env bash
set -euo pipefail
{
    printf 'codex|pwd=%s|args=' "$(pwd)"
    printf '%q ' "$@"
    printf '\n'
} >>"${FAKE_TUI_HELPER_LOG:?FAKE_TUI_HELPER_LOG is required}"
printf 'ws://127.0.0.1:3210\n'
exec sleep 30
EOF_FAKE_CODEX
cat >"${CURRENT_TARGET_DIR}/debug/codex-tui" <<'EOF_FAKE_CODEX_TUI'
#!/usr/bin/env bash
set -euo pipefail
{
    printf 'codex-tui|pwd=%s|url=%s|args=' \
        "$(pwd)" \
        "${CODEX_EXEC_SERVER_URL:-unset}"
    printf '%q ' "$@"
    printf '\n'
} >>"${FAKE_TUI_HELPER_LOG:?FAKE_TUI_HELPER_LOG is required}"
EOF_FAKE_CODEX_TUI
chmod +x "${CURRENT_TARGET_DIR}/debug/codex" "${CURRENT_TARGET_DIR}/debug/codex-tui"
export FAKE_TUI_HELPER_LOG="${helper_log}"
export CODEX_EXEC_SERVER_START_TIMEOUT_SECONDS=2
expect_command_ok just tui-with-exec-server smoke-arg
unset FAKE_TUI_HELPER_LOG CODEX_EXEC_SERVER_START_TIMEOUT_SECONDS
assert_file_contains "${CURRENT_LOG}" 'args=build -p codex-cli --bin codex -p codex-tui --bin codex-tui '
assert_file_not_contains "${CURRENT_LOG}" 'args=run( |$)'
assert_file_contains "${helper_log}" 'codex\|pwd=.*/codex-rs\|args=exec-server --listen ws://127[.]0[.]0[.]1:0 '
assert_file_contains "${helper_log}" 'codex-tui\|pwd=.*/codex-rs\|url=ws://127[.]0[.]0[.]1:3210\|args=-c mcp_oauth_credentials_store=file smoke-arg '

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=missing
expect_fail cargo check -p codex-core
assert_file_contains "${CURRENT_OUT}" 'failed to load resource profile missing'
assert_file_empty "${CURRENT_LOG}"

begin_case
expect_ok cargo check -p codex-core
assert_file_contains "${CURRENT_LOG}" 'args=metadata .*'
assert_file_contains "${CURRENT_LOG}" 'args=check -p codex-core '

begin_case
expect_fail cargo check -j 5 -p codex-core
assert_file_contains "${CURRENT_OUT}" 'exceeds selected cap 4'
assert_file_not_contains "${CURRENT_LOG}" 'args=check -j 5 -p codex-core '

begin_case
expect_fail cargo check --jobs=5 -p codex-core
assert_file_contains "${CURRENT_OUT}" 'exceeds selected cap 4'

begin_case
expect_fail cargo check --config build.jobs=8 -p codex-core
assert_file_contains "${CURRENT_OUT}" 'Cargo build.jobs config 8 exceeds selected cap 4'

begin_case
expect_fail cargo check --config 'build.jobs=2;build.jobs=99' -p codex-core
assert_file_contains "${CURRENT_OUT}" 'Cargo build.jobs config was specified more than once'
assert_file_not_contains "${CURRENT_LOG}" 'args=check --config build.jobs=2'

begin_case
expect_ok cargo check --config build.jobs=2 -p codex-core
assert_file_contains "${CURRENT_LOG}" 'args=check --config build.jobs=2 -p codex-core \|jobs=2\|stack=unset\|rust_threads=unset\|nextest_threads=unset\|target_env=unset$'

begin_case
expect_fail cargo check --config path/to/config.toml -p codex-core
assert_file_contains "${CURRENT_OUT}" 'path-style --config path/to/config.toml is rejected'

begin_case
expect_fail cargo check --config include=other.toml -p codex-core
assert_file_contains "${CURRENT_OUT}" 'include-based --config include=other.toml is rejected'

begin_case
expect_ok cargo check -p codex-core
assert_file_contains "${CURRENT_LOG}" 'args=check -p codex-core \|jobs=4\|stack=unset\|rust_threads=unset\|nextest_threads=unset\|target_env=unset$'

begin_case
export CARGO_BUILD_JOBS=999
expect_ok cargo check -p codex-core
assert_file_contains "${CURRENT_LOG}" 'args=metadata .*\|jobs=unset\|stack=unset\|rust_threads=unset\|nextest_threads=unset'
assert_file_contains "${CURRENT_LOG}" 'args=check -p codex-core \|jobs=4\|stack=unset\|rust_threads=unset\|nextest_threads=unset\|target_env=unset$'

begin_case
expect_ok cargo check -j 2 -p codex-core
assert_file_contains "${CURRENT_LOG}" 'args=check -j 2 -p codex-core \|jobs=2\|stack=unset\|rust_threads=unset\|nextest_threads=unset\|target_env=unset$'

begin_case
expect_fail cargo check -j 99 -j 2 -p codex-core
assert_file_contains "${CURRENT_OUT}" 'Cargo build-job count was specified more than once'
assert_file_not_contains "${CURRENT_LOG}" 'args=check -j 99 -j 2 -p codex-core '

begin_case
expect_fail cargo check --config build.jobs=99 --config build.jobs=2 -p codex-core
assert_file_contains "${CURRENT_OUT}" 'Cargo build-job count was specified more than once'
assert_file_not_contains "${CURRENT_LOG}" 'args=check --config build.jobs=99 --config build.jobs=2 -p codex-core '

begin_case
expect_fail cargo check -j 2 --config build.jobs=2 -p codex-core
assert_file_contains "${CURRENT_OUT}" 'Cargo build-job count was specified more than once'
assert_file_not_contains "${CURRENT_LOG}" 'args=check -j 2 --config build.jobs=2 -p codex-core '

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=check
expect_ok cargo check -p codex-core
assert_file_contains "${CURRENT_OUT}" 'jobs-mode: auto'
assert_file_contains "${CURRENT_OUT}" 'cargo-build-jobs: selected=4 cap=16 .*default=min source=min'
assert_file_contains "${CURRENT_LOG}" 'args=check -p codex-core \|jobs=4\|stack=unset\|rust_threads=unset\|nextest_threads=unset\|target_env=unset$'

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=check
expect_ok cargo check -j 3 -p codex-core
assert_file_contains "${CURRENT_OUT}" 'cargo-build-jobs: selected=3 cap=16 .*source=explicit'
assert_file_contains "${CURRENT_LOG}" 'args=check -j 3 -p codex-core \|jobs=3\|stack=unset\|rust_threads=unset\|nextest_threads=unset\|target_env=unset$'

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=check
expect_ok cargo check --config build.jobs=2 -p codex-core
assert_file_contains "${CURRENT_LOG}" 'args=check --config build.jobs=2 -p codex-core \|jobs=2\|stack=unset\|rust_threads=unset\|nextest_threads=unset\|target_env=unset$'

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=check
expect_ok cargo check --config build.jobs=3 -p codex-core
assert_file_contains "${CURRENT_OUT}" 'cargo-build-jobs: selected=3 cap=16 .*source=config'
assert_file_contains "${CURRENT_LOG}" 'args=check --config build.jobs=3 -p codex-core \|jobs=3\|stack=unset\|rust_threads=unset\|nextest_threads=unset\|target_env=unset$'

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=check
export FAKE_NPROC_VALUE=0
expect_fail cargo check -p codex-core
assert_file_contains "${CURRENT_OUT}" 'nproc must return a positive integer'
assert_file_not_contains "${CURRENT_LOG}" 'args=check -p codex-core '

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=check
override_nproc="${TMP_ROOT}/nproc-override"
cat >"${override_nproc}" <<'EOF_NPROC_OVERRIDE'
#!/usr/bin/env bash
printf '12\n'
EOF_NPROC_OVERRIDE
chmod +x "${override_nproc}"
export CARGO_GUARD_NPROC_CMD="${override_nproc}"
expect_ok cargo check -p codex-core
assert_file_contains "${CURRENT_OUT}" 'job-inputs: nproc=12 cpu-cap=5'
assert_file_contains "${CURRENT_LOG}" 'args=check -p codex-core \|jobs=4\|stack=unset\|rust_threads=unset\|nextest_threads=unset\|target_env=unset$'

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=check
printf 'MemTotal: 1 kB\n' >"${CURRENT_MEMINFO}"
expect_fail cargo check -p codex-core
assert_file_contains "${CURRENT_OUT}" 'MemAvailable must be present as an integer'
assert_file_not_contains "${CURRENT_LOG}" 'args=check -p codex-core '

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=check
printf 'MemAvailable: 1024 kB\n' >"${CURRENT_MEMINFO}"
expect_fail cargo check -p codex-core
assert_file_contains "${CURRENT_OUT}" 'adaptive Cargo jobs cap 0 is below profile minimum 4'
assert_file_not_contains "${CURRENT_LOG}" 'args=check -p codex-core '

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=check
printf 'MemAvailable: %s kB\n' $((10752 * 1024)) >"${CURRENT_MEMINFO}"
expect_fail cargo check -p codex-core
assert_file_contains "${CURRENT_OUT}" 'adaptive Cargo jobs cap 2 is below profile minimum 4'
assert_file_not_contains "${CURRENT_LOG}" 'args=check -p codex-core '

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=check
printf 'MemAvailable: %s kB\n' $((10752 * 1024)) >"${CURRENT_MEMINFO}"
expect_ok cargo check -j 2 -p codex-core
assert_file_contains "${CURRENT_OUT}" 'cargo-build-jobs: selected=2 cap=2 .*source=explicit'
assert_file_contains "${CURRENT_LOG}" 'args=check -j 2 -p codex-core \|jobs=2\|stack=unset\|rust_threads=unset\|nextest_threads=unset\|target_env=unset$'

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=check
export CARGO_GUARD_JOBS_DEFAULT=auto
printf 'MemAvailable: 1024 kB\n' >"${CURRENT_MEMINFO}"
expect_fail cargo check -p codex-core
assert_file_contains "${CURRENT_OUT}" 'selected Cargo job cap 0 is below 1'
assert_file_not_contains "${CURRENT_LOG}" 'args=check -p codex-core '

begin_case
expect_ok cargo test -p codex-core
assert_file_contains "${CURRENT_LOG}" 'args=test -p codex-core \|jobs=4\|stack=8388608\|rust_threads=unset\|nextest_threads=unset\|target_env=unset$'

begin_case
RUST_MIN_STACK=1234 expect_ok cargo test -p codex-core
assert_file_contains "${CURRENT_LOG}" 'args=test -p codex-core \|jobs=4\|stack=1234\|rust_threads=unset\|nextest_threads=unset\|target_env=unset$'

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=package_test
export RUST_TEST_THREADS=999
expect_ok cargo test -p codex-core
assert_file_contains "${CURRENT_LOG}" 'args=test -p codex-core -- --test-threads=1 \|jobs=4\|stack=8388608\|rust_threads=1\|nextest_threads=unset\|target_env=unset$'

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=package_test
export CARGO_GUARD_EXPECTED_GROWTH_GIB=40
export FAKE_DF_TOTAL_GIB=200
export FAKE_DF_DEFAULT_GIB=70
expect_ok cargo test -p codex-core
assert_file_contains "${CURRENT_OUT}" 'low-disk-clamp=1'
assert_file_contains "${CURRENT_LOG}" 'args=test -p codex-core -- --test-threads=1 \|jobs=4\|stack=8388608\|rust_threads=1\|nextest_threads=unset\|target_env=unset$'

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=package_test
expect_ok cargo test -p codex-core -- --test-threads=1
assert_file_contains "${CURRENT_LOG}" 'args=test -p codex-core -- --test-threads=1 \|jobs=4\|stack=8388608\|rust_threads=1\|nextest_threads=unset\|target_env=unset$'

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=package_test
expect_fail cargo test -p codex-core -- --test-threads=5
assert_file_contains "${CURRENT_OUT}" 'cargo test runtime thread count 5 exceeds cap 1'
assert_file_not_contains "${CURRENT_LOG}" 'args=test -p codex-core '

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=package_test
expect_fail cargo test -p codex-core -- --test-threads=1 --test-threads=1
assert_file_contains "${CURRENT_OUT}" 'cargo test runtime thread count was specified more than once'
assert_file_not_contains "${CURRENT_LOG}" 'args=test -p codex-core '

begin_case
expect_ok cargo check --manifest-path codex-rs/Cargo.toml -p codex-core
assert_file_contains "${CURRENT_LOG}" "args=metadata .*--manifest-path ${REPO_ROOT}/codex-rs/Cargo.toml"

begin_case
outside_dir="${TMP_ROOT}/outside"
mkdir -p -- "${outside_dir}"
printf '[package]\nname = "outside"\nversion = "0.0.0"\nedition = "2021"\n' >"${outside_dir}/Cargo.toml"
expect_fail cargo check --manifest-path "${outside_dir}/Cargo.toml" -p codex-core
assert_file_contains "${CURRENT_OUT}" 'must stay under .*/codex-rs'

begin_case
explicit_target="${TMP_ROOT}/explicit-target"
export FAKE_STAT_TARGET_PATH="${explicit_target}"
expect_ok cargo check --target-dir "${explicit_target}" -p codex-core
assert_file_contains "${CURRENT_LOG}" "args=metadata .*\|jobs=unset\|stack=unset\|rust_threads=unset\|nextest_threads=unset\|target_env=${explicit_target}$"

begin_case
export FAKE_CARGO_OMIT_BUILD_DIR_JSON=1
expect_ok cargo check -p codex-core
assert_file_contains "${CURRENT_OUT}" "build-dir: ${CURRENT_TARGET_DIR}"

begin_case
metrics_path="${TMP_ROOT}/metrics.json"
export CARGO_GUARD_RESOURCE_PROFILE=check
export CARGO_GUARD_METRICS_PATH="${metrics_path}"
export CARGO_GUARD_COMMAND_FINGERPRINT=abc123
expect_ok cargo check -p codex-core
python3 - "${metrics_path}" <<'PY'
import json
import sys
from pathlib import Path

metrics = json.loads(Path(sys.argv[1]).read_text())
assert "fingerprint" not in metrics
assert "selected_jobs" not in metrics
assert "paths" not in metrics
assert "low_disk_clamp" not in metrics
assert "observed_growth_bytes" not in metrics
assert metrics["command_fingerprint"] == "abc123"
assert metrics["job_contract_digest"]
assert metrics["resource_profile"] == "check"
assert metrics["cargo_subcommand"] == "check"
assert metrics["jobs_default"] == "min"
assert metrics["selected_cargo_build_job_cap"] == 16
assert metrics["effective_cargo_build_jobs"] == 4
assert metrics["effective_cargo_build_jobs_source"] == "min"
assert metrics["selected_runtime_test_threads"] is None
assert metrics["observed_growth_gib"] == 0
assert metrics["telemetry_level"] == "off"
assert metrics["telemetry_log_path"] is None
assert metrics["telemetry_sample_count"] == 0
assert metrics["telemetry_error_count"] == 0
assert metrics["top_rustc_crates"] == []
assert any(path["label"] == "target" and "fs_id" in path for path in metrics["monitored_paths"])
PY
if [[ -e "${CURRENT_HISTORY}" ]]; then
    fail "direct history should not be appended when validator metrics path owns the run"
fi

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=check
expect_ok cargo check -p codex-core
python3 - "${CURRENT_HISTORY}" <<'PY'
import json
import sys
from pathlib import Path

entries = [json.loads(line) for line in Path(sys.argv[1]).read_text().splitlines()]
assert len(entries) == 1
entry = entries[0]
assert entry["argv"] == ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-core"]
assert entry["resource_profile"] == "check"
assert entry["risk_kind"] == "success"
assert entry["status"] == 0
assert entry["disk_emergency"] is False
assert entry["observed_growth_gib"] == 0
assert entry["selected_jobs"] == 4
assert entry["selected_jobs_source"] == "min"
assert entry["jobs_default"] == "min"
assert entry["job_contract_digest"]
assert entry["test_threads"] is None
PY

begin_case
metrics_path="${TMP_ROOT}/transient-growth-metrics.json"
export CARGO_GUARD_METRICS_PATH="${metrics_path}"
export CARGO_GUARD_COMMAND_FINGERPRINT=transient-growth
export CARGO_GUARD_MONITOR=1
export CARGO_GUARD_MONITOR_INTERVAL_SECS=1
export FAKE_CARGO_COMMAND_SLEEP=2
set_df_sequence 99 99 99 99 99 80 99 99 99 99 99 99
expect_ok cargo check -p codex-core
python3 - "${metrics_path}" <<'PY'
import json
import sys
from pathlib import Path

metrics = json.loads(Path(sys.argv[1]).read_text())
target_paths = [path for path in metrics["monitored_paths"] if path["label"] == "target"]
assert len(target_paths) == 1
assert target_paths[0]["observed_growth_bytes"] == 19 * 1073741824
assert metrics["observed_growth_gib"] == 19
PY

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=check
export CARGO_GUARD_METRICS_PATH="${TMP_ROOT}/missing-fingerprint.json"
expect_fail cargo check -p codex-core
assert_file_contains "${CURRENT_OUT}" 'CARGO_GUARD_METRICS_PATH requires CARGO_GUARD_COMMAND_FINGERPRINT'
assert_file_not_contains "${CURRENT_LOG}" 'args=metadata '

begin_case
export CARGO_GUARD_TELEMETRY_LEVEL=full
expect_fail cargo check -p codex-core
assert_file_contains "${CURRENT_OUT}" 'CARGO_GUARD_TELEMETRY_LEVEL=full requires CARGO_GUARD_TELEMETRY_PATH'
assert_file_not_contains "${CURRENT_LOG}" 'args=metadata '

begin_case
metrics_path="${TMP_ROOT}/telemetry-metrics.json"
telemetry_path="${TMP_ROOT}/telemetry/debug.tsv"
telemetry_line_count_at_start="${TMP_ROOT}/telemetry/line-count-at-start.txt"
process_list="${TMP_ROOT}/telemetry-processes.txt"
cat >"${process_list}" <<'EOF_TELEMETRY_PS'
cargo 512 cargo check -p codex-core
rustc 8192 /tmp/private/rustc --crate-name codex_core --edition 2021 --crate-type lib /tmp/private/src/lib.rs
rustc 4096 rustc --crate-name codex_cli --edition=2021 --crate-type bin /tmp/private/src/main.rs
ld.lld 1024 ld.lld
build-script-build 256 build-script-build
EOF_TELEMETRY_PS
export FAKE_PS_PROCESS_LIST_FILE="${process_list}"
export CARGO_GUARD_RESOURCE_PROFILE=check
export CARGO_GUARD_METRICS_PATH="${metrics_path}"
export CARGO_GUARD_COMMAND_FINGERPRINT=telemetry-debug
export CARGO_GUARD_TELEMETRY_LEVEL=debug
export CARGO_GUARD_TELEMETRY_PATH="${telemetry_path}"
export FAKE_CARGO_TELEMETRY_LINE_COUNT_FILE="${telemetry_line_count_at_start}"
expect_ok cargo check -p codex-core
python3 - "${metrics_path}" "${telemetry_path}" "${telemetry_line_count_at_start}" <<'PY'
import csv
import json
import sys
from pathlib import Path

metrics = json.loads(Path(sys.argv[1]).read_text())
telemetry_path = Path(sys.argv[2])
line_count_at_start = int(Path(sys.argv[3]).read_text())
rows = list(csv.DictReader(telemetry_path.open(encoding="utf-8"), delimiter="\t"))
assert metrics["telemetry_level"] == "debug"
assert metrics["telemetry_log_path"] == str(telemetry_path)
assert metrics["telemetry_sample_count"] >= 1
assert metrics["telemetry_error_count"] == 0
assert metrics["top_rustc_crates"][0] == {
    "crate_name": "codex_core",
    "max_rss_kib": 8192,
    "samples": 1,
    "sum_rss_kib": 8192,
}
aggregate_rows = [row for row in rows if row["row_type"] == "aggregate"]
detail_rows = [row for row in rows if row["row_type"] == "detail"]
assert aggregate_rows
assert aggregate_rows[0]["jobs_selected"] == "4"
assert aggregate_rows[0]["jobs_cap"] == "16"
assert aggregate_rows[0]["jobs_default"] == "min"
assert aggregate_rows[0]["jobs_source"] == "min"
assert any(row["crate_name"] == "codex_core" for row in detail_rows)
assert all("/tmp/private" not in row["args_preview"] for row in detail_rows)
assert line_count_at_start >= 2
PY

begin_case
metrics_path="${TMP_ROOT}/telemetry-sample-error-metrics.json"
telemetry_path="${TMP_ROOT}/telemetry/sample-error.tsv"
export FAKE_PS_FAIL_PROCESS_LIST=1
export CARGO_GUARD_RESOURCE_PROFILE=check
export CARGO_GUARD_METRICS_PATH="${metrics_path}"
export CARGO_GUARD_COMMAND_FINGERPRINT=telemetry-sample-error
export CARGO_GUARD_TELEMETRY_LEVEL=full
export CARGO_GUARD_TELEMETRY_PATH="${telemetry_path}"
expect_ok cargo check -p codex-core
python3 - "${metrics_path}" "${telemetry_path}" <<'PY'
import csv
import json
import sys
from pathlib import Path

metrics = json.loads(Path(sys.argv[1]).read_text())
rows = list(csv.DictReader(Path(sys.argv[2]).open(encoding="utf-8"), delimiter="\t"))
assert metrics["telemetry_sample_count"] >= 1
assert metrics["telemetry_error_count"] == 1
assert any("ps failed: forced process-list failure" in row["sample_error"] for row in rows)
PY

begin_case
metrics_path="${TMP_ROOT}/telemetry-periodic-failure-metrics.json"
telemetry_path="${TMP_ROOT}/periodic-telemetry/periodic.tsv"
export CARGO_GUARD_RESOURCE_PROFILE=check
export CARGO_GUARD_METRICS_PATH="${metrics_path}"
export CARGO_GUARD_COMMAND_FINGERPRINT=telemetry-periodic-failure
export CARGO_GUARD_TELEMETRY_LEVEL=full
export CARGO_GUARD_TELEMETRY_PATH="${telemetry_path}"
export CARGO_GUARD_MONITOR_INTERVAL_SECS=1
export FAKE_CARGO_COMMAND_SLEEP=2
export FAKE_CARGO_BREAK_TELEMETRY_AFTER_START=1
expect_ok cargo check -p codex-core
python3 - "${metrics_path}" "${telemetry_path}" <<'PY'
import json
import sys
from pathlib import Path

metrics = json.loads(Path(sys.argv[1]).read_text())
assert metrics["status"] == 0
assert metrics["telemetry_level"] == "full"
assert metrics["telemetry_log_path"] == sys.argv[2]
assert metrics["telemetry_error_count"] >= 1
PY

begin_case
metrics_path="${TMP_ROOT}/telemetry-init-failed-metrics.json"
telemetry_parent="${TMP_ROOT}/telemetry-parent-file"
telemetry_path="${telemetry_parent}/child.tsv"
: >"${telemetry_parent}"
export CARGO_GUARD_RESOURCE_PROFILE=check
export CARGO_GUARD_METRICS_PATH="${metrics_path}"
export CARGO_GUARD_COMMAND_FINGERPRINT=telemetry-init-failed
export CARGO_GUARD_TELEMETRY_LEVEL=full
export CARGO_GUARD_TELEMETRY_PATH="${telemetry_path}"
expect_fail cargo check -p codex-core
assert_file_contains "${CURRENT_OUT}" 'failed to initialize Cargo guard telemetry'
assert_file_not_contains "${CURRENT_LOG}" 'args=check -p codex-core '
python3 - "${metrics_path}" "${telemetry_path}" <<'PY'
import json
import sys
from pathlib import Path

metrics = json.loads(Path(sys.argv[1]).read_text())
assert metrics["status"] == 1
assert metrics["telemetry_level"] == "full"
assert metrics["telemetry_log_path"] == sys.argv[2]
assert metrics["telemetry_sample_count"] == 0
assert metrics["telemetry_error_count"] == 1
assert metrics["top_rustc_crates"] == []
PY

begin_case
export FAKE_CARGO_COMMAND_STATUS=7
expect_fail cargo check -p codex-core
assert_clean_count 1
assert_package_clean_count codex-core 1
assert_order 'args=check -p codex-core ' 'args=clean -p codex-core '
assert_file_contains "${CURRENT_OUT}" 'running targeted cargo clean: cargo clean -p codex-core \(failed package artifacts for nonzero rung\)'
assert_file_contains "${CURRENT_OUT}" 'command exited with status 7, but monitored free space is above reserve; skipping cargo clean'

begin_case
set_df_sequence 99 1 99 99 99 99 99 99
expect_ok cargo check --workspace
assert_clean_count 1
assert_order 'args=clean ' 'args=check --workspace '
assert_file_not_contains "${CURRENT_LOG}" 'args=clean .* -p '

begin_case
set_df_sequence 99 1 99 99
expect_fail cargo check -p codex-core
assert_clean_count 0
assert_file_contains "${CURRENT_OUT}" 'pre-run headroom is below threshold before a package-targeted command; preserving package caches'

begin_case
export CARGO_GUARD_NO_CLEAN=1
set_df_sequence 99 1 99 99
expect_fail cargo check --workspace
assert_clean_count 0
assert_file_contains "${CURRENT_OUT}" 'CARGO_GUARD_NO_CLEAN=1 forbids cargo clean \(pre-run cleanable filesystem below required start headroom\)'

begin_case
set_df_sequence 1 1 99 99 99 99 99 99
expect_ok cargo check --workspace
assert_clean_count 1
assert_file_contains "${CURRENT_OUT}" 'required free-space failure: workspace='
assert_file_contains "${CURRENT_OUT}" 'required free-space failure: target='

begin_case
set_df_sequence 1 99 99 99
expect_fail cargo check --workspace
assert_clean_count 0
assert_file_contains "${CURRENT_OUT}" 'required start headroom is unavailable only on non-cleanable monitored filesystems'

begin_case
set_df_sequence 99 99 99 1
expect_fail cargo check --workspace
assert_clean_count 0
assert_file_contains "${CURRENT_OUT}" 'required free-space failure: cargo-home='
assert_file_contains "${CURRENT_OUT}" 'required start headroom is unavailable only on non-cleanable monitored filesystems'

begin_case
set_df_sequence 99 99 99 99 99 1 99 99 99 99 99 99
expect_ok cargo check --workspace
assert_clean_count 1
assert_order 'args=check --workspace ' 'args=clean '

begin_case
export CARGO_GUARD_NO_CLEAN=1
set_df_sequence 99 99 99 99 99 1 99 99
expect_fail cargo check --workspace
assert_clean_count 0
assert_file_contains "${CURRENT_OUT}" 'CARGO_GUARD_NO_CLEAN=1 forbids cargo clean \(post-run cleanable filesystem below reserve\)'

begin_case
export CARGO_GUARD_NO_POST_CLEAN=1
set_df_sequence 99 99 99 99 99 1 99 99
expect_fail cargo check --workspace
assert_clean_count 0
assert_file_contains "${CURRENT_OUT}" 'CARGO_GUARD_NO_POST_CLEAN=1 forbids cargo clean \(post-run cleanable filesystem below reserve\)'

begin_case
export CARGO_GUARD_NO_POST_CLEAN=1
set_df_sequence 99 1 99 99 99 99 99 99
expect_ok cargo check --workspace
assert_clean_count 1
assert_order 'args=clean ' 'args=check --workspace '

begin_case
set_df_sequence 99 99 99 99 99 1 99 99 99 1 99 99
expect_fail cargo check --workspace
assert_clean_count 1
assert_file_contains "${CURRENT_OUT}" 'post-run reserve is unavailable only on non-cleanable monitored filesystems|free-space failure'

begin_case
CURRENT_BUILD_DIR="${TMP_ROOT}/outside-build"
export FAKE_CARGO_BUILD_DIR_JSON="${CURRENT_BUILD_DIR}"
set_df_sequence 99 99 1 99 99
expect_fail cargo check --workspace
assert_clean_count 0
assert_file_contains "${CURRENT_OUT}" 'required free-space failure: build='
assert_file_contains "${CURRENT_OUT}" 'required start headroom is unavailable only on non-cleanable monitored filesystems'

begin_case
CURRENT_BUILD_DIR="${CURRENT_TARGET_DIR}/build-cache"
export FAKE_CARGO_BUILD_DIR_JSON="${CURRENT_BUILD_DIR}"
set_df_sequence 99 99 1 99 99 99 99 99 99 99
expect_ok cargo check --workspace
assert_clean_count 1
assert_file_contains "${CURRENT_OUT}" "guard-path: build=${CURRENT_BUILD_DIR} .*clean-candidate=1"

begin_case
explicit_target="${TMP_ROOT}/explicit-clean-target"
export FAKE_STAT_TARGET_PATH="${explicit_target}"
set_df_sequence 99 1 99 99 99 99 99 99
expect_ok cargo check --target-dir "${explicit_target}" --workspace
assert_file_contains "${CURRENT_LOG}" "args=clean --target-dir ${explicit_target} "
assert_file_not_contains "${CURRENT_LOG}" "args=clean .* -p "

begin_case
mkdir -p -- "${CURRENT_WORKSPACE_TARGET_DIR}/debug"
printf 'stale artifact\n' >"${CURRENT_WORKSPACE_TARGET_DIR}/debug/stale-bin"
set_df_sequence 99 99 1 99 99 99 99 99 99 99
expect_ok cargo check --workspace
assert_clean_count 1
assert_file_contains "${CURRENT_OUT}" "guard-path: stale-target:workspace=${CURRENT_WORKSPACE_TARGET_DIR} .*clean-candidate=1"
assert_file_contains "${CURRENT_OUT}" "cleaning stale target cache: stale-target:workspace=${CURRENT_WORKSPACE_TARGET_DIR}"
if [[ -e "${CURRENT_WORKSPACE_TARGET_DIR}/debug/stale-bin" ]]; then
    fail "expected stale workspace target contents to be removed"
fi

begin_case
mkdir -p -- "${CURRENT_WORKSPACE_TARGET_DIR}/debug"
printf 'active artifact\n' >"${CURRENT_WORKSPACE_TARGET_DIR}/debug/active-bin"
CURRENT_TARGET_DIR="${CURRENT_WORKSPACE_TARGET_DIR}"
CURRENT_BUILD_DIR="${CURRENT_TARGET_DIR}"
export FAKE_CARGO_TARGET_DIR_JSON="${CURRENT_TARGET_DIR}"
export FAKE_CARGO_BUILD_DIR_JSON="${CURRENT_BUILD_DIR}"
export FAKE_STAT_TARGET_PATH="${CURRENT_TARGET_DIR}"
set_df_sequence 99 1 99 99 99 99 99 99
expect_ok cargo check --workspace
assert_clean_count 1
assert_file_not_contains "${CURRENT_OUT}" "cleaning stale target cache: stale-target:workspace=${CURRENT_WORKSPACE_TARGET_DIR}"
if [[ ! -e "${CURRENT_WORKSPACE_TARGET_DIR}/debug/active-bin" ]]; then
    fail "workspace target contents were removed while it was the effective target"
fi

begin_case
mkdir -p -- "${CURRENT_SHARED_TARGET_DIR}/debug"
printf 'stale artifact\n' >"${CURRENT_SHARED_TARGET_DIR}/debug/stale-bin"
set_df_sequence 99 99 1 99 99 99 99 99 99 99
expect_ok cargo check --workspace
assert_clean_count 1
assert_file_contains "${CURRENT_OUT}" "guard-path: stale-target:shared=${CURRENT_SHARED_TARGET_DIR} .*clean-candidate=1"
assert_file_contains "${CURRENT_OUT}" "cleaning stale target cache: stale-target:shared=${CURRENT_SHARED_TARGET_DIR}"
if [[ -e "${CURRENT_SHARED_TARGET_DIR}/debug/stale-bin" ]]; then
    fail "expected stale shared target contents to be removed"
fi

begin_case
mkdir -p -- "${CURRENT_SHARED_TARGET_DIR}/debug"
printf 'stale artifact\n' >"${CURRENT_SHARED_TARGET_DIR}/debug/stale-bin"
export CARGO_GUARD_NO_CLEAN=1
set_df_sequence 99 99 1 99 99
expect_fail cargo check --workspace
assert_clean_count 0
assert_file_contains "${CURRENT_OUT}" 'CARGO_GUARD_NO_CLEAN=1 forbids cargo clean \(pre-run cleanable filesystem below required start headroom\)'
if [[ ! -e "${CURRENT_SHARED_TARGET_DIR}/debug/stale-bin" ]]; then
    fail "stale shared target contents were removed despite no-clean mode"
fi

begin_case
mkdir -p -- "${CURRENT_SHARED_TARGET_DIR}/debug"
printf 'active artifact\n' >"${CURRENT_SHARED_TARGET_DIR}/debug/active-bin"
CURRENT_TARGET_DIR="${CURRENT_SHARED_TARGET_DIR}"
CURRENT_BUILD_DIR="${CURRENT_TARGET_DIR}"
export FAKE_CARGO_TARGET_DIR_JSON="${CURRENT_TARGET_DIR}"
export FAKE_CARGO_BUILD_DIR_JSON="${CURRENT_BUILD_DIR}"
export FAKE_STAT_TARGET_PATH="${CURRENT_TARGET_DIR}"
set_df_sequence 99 1 99 99 99 99 99 99
expect_ok cargo check --workspace
assert_clean_count 1
assert_file_not_contains "${CURRENT_OUT}" "cleaning stale target cache: stale-target:shared=${CURRENT_SHARED_TARGET_DIR}"
if [[ ! -e "${CURRENT_SHARED_TARGET_DIR}/debug/active-bin" ]]; then
    fail "shared target contents were removed while it was the effective target"
fi

begin_case
mkdir -p -- "${CURRENT_SHARED_TARGET_DIR%/*}" "${TMP_ROOT}/linked-shared-target/debug"
printf 'linked artifact\n' >"${TMP_ROOT}/linked-shared-target/debug/linked-bin"
ln -s "${TMP_ROOT}/linked-shared-target" "${CURRENT_SHARED_TARGET_DIR}"
set_df_sequence 99 1 99 99 99 99 99 99
expect_ok cargo check --workspace
assert_clean_count 1
assert_file_not_contains "${CURRENT_OUT}" "guard-path: stale-target:shared=${CURRENT_SHARED_TARGET_DIR}"
if [[ ! -e "${TMP_ROOT}/linked-shared-target/debug/linked-bin" ]]; then
    fail "symlinked shared target contents were removed"
fi

begin_case
unknown_cache="${CURRENT_SHARED_TARGET_DIR%/*}/other-workspace"
mkdir -p -- "${unknown_cache}/debug"
printf 'unknown artifact\n' >"${unknown_cache}/debug/unknown-bin"
set_df_sequence 99 1 99 99 99 99 99 99
expect_ok cargo check --workspace
assert_clean_count 1
assert_file_not_contains "${CURRENT_OUT}" "other-workspace"
if [[ ! -e "${unknown_cache}/debug/unknown-bin" ]]; then
    fail "unknown cargo-target sibling was removed"
fi

begin_case
mkdir -p -- "${CURRENT_SHARED_TARGET_DIR}/debug"
printf 'broad artifact\n' >"${CURRENT_SHARED_TARGET_DIR}/debug/broad-bin"
export CARGO_GUARD_TEST_SHARED_TARGET_DIR="${TMP_ROOT}"
set_df_sequence 99 1 99 99 99 99 99 99
expect_fail cargo check -p codex-core
assert_clean_count 0
assert_file_contains "${CURRENT_OUT}" 'CARGO_GUARD_TEST_SHARED_TARGET_DIR must end with /cargo-target/codex-rs'
if [[ ! -e "${CURRENT_SHARED_TARGET_DIR}/debug/broad-bin" ]]; then
    fail "shared target contents were removed through broad override"
fi

begin_case
export CARGO_GUARD_TEST_THREADS_MAX=4
expect_ok cargo nextest run --no-fail-fast
assert_file_contains "${CURRENT_LOG}" 'args=nextest run --no-fail-fast --build-jobs 4 --test-threads 4 '

begin_case
export CARGO_GUARD_TEST_THREADS_MAX=4
expect_ok cargo --config build.jobs=2 nextest run
assert_file_contains "${CURRENT_LOG}" 'args=--config build.jobs=2 nextest run --build-jobs 2 --test-threads 4 \|jobs=2\|stack=unset\|rust_threads=unset\|nextest_threads=4\|target_env=unset$'

begin_case
expect_fail cargo --config build.jobs=2 nextest run --build-jobs 2
assert_file_contains "${CURRENT_OUT}" 'Cargo build-job count was specified more than once'
assert_file_not_contains "${CURRENT_LOG}" 'args=--config build.jobs=2 nextest run --build-jobs 2 '

begin_case
export CARGO_GUARD_TEST_THREADS_MAX=8
expect_ok cargo nextest run -j8
assert_file_contains "${CURRENT_LOG}" 'args=nextest run -j8 --build-jobs 4 \|jobs=4\|stack=unset\|rust_threads=unset\|nextest_threads=8\|target_env=unset$'

begin_case
export CARGO_GUARD_TEST_THREADS_MAX=8
expect_ok cargo nextest run -j 8
assert_file_contains "${CURRENT_LOG}" 'args=nextest run -j 8 --build-jobs 4 \|jobs=4\|stack=unset\|rust_threads=unset\|nextest_threads=8\|target_env=unset$'

begin_case
export CARGO_GUARD_TEST_THREADS_MAX=8
expect_ok cargo nextest run --jobs=8
assert_file_contains "${CURRENT_LOG}" 'args=nextest run --jobs=8 --build-jobs 4 \|jobs=4\|stack=unset\|rust_threads=unset\|nextest_threads=8\|target_env=unset$'

begin_case
export CARGO_GUARD_TEST_THREADS_MAX=8
expect_ok cargo nextest run --jobs 8
assert_file_contains "${CURRENT_LOG}" 'args=nextest run --jobs 8 --build-jobs 4 \|jobs=4\|stack=unset\|rust_threads=unset\|nextest_threads=8\|target_env=unset$'

begin_case
export CARGO_GUARD_TEST_THREADS_MAX=8
expect_ok cargo nextest run --test-threads=8
assert_file_contains "${CURRENT_LOG}" 'args=nextest run --test-threads=8 --build-jobs 4 \|jobs=4\|stack=unset\|rust_threads=unset\|nextest_threads=8\|target_env=unset$'

begin_case
export CARGO_GUARD_TEST_THREADS_MAX=8
expect_ok cargo nextest run --test-threads 8
assert_file_contains "${CURRENT_LOG}" 'args=nextest run --test-threads 8 --build-jobs 4 \|jobs=4\|stack=unset\|rust_threads=unset\|nextest_threads=8\|target_env=unset$'

begin_case
expect_ok cargo nextest run --build-jobs=2
assert_file_contains "${CURRENT_LOG}" 'args=nextest run --build-jobs=2 \|jobs=2\|stack=unset\|rust_threads=unset\|nextest_threads=unset\|target_env=unset$'

begin_case
expect_ok cargo nextest run --build-jobs 2
assert_file_contains "${CURRENT_LOG}" 'args=nextest run --build-jobs 2 \|jobs=2\|stack=unset\|rust_threads=unset\|nextest_threads=unset\|target_env=unset$'

begin_case
export CARGO_GUARD_TEST_THREADS_MAX=4
export NEXTEST_TEST_THREADS=999
expect_ok cargo nextest run
assert_file_contains "${CURRENT_LOG}" 'args=nextest run --build-jobs 4 --test-threads 4 \|jobs=4\|stack=unset\|rust_threads=unset\|nextest_threads=4\|target_env=unset$'

begin_case
expect_fail cargo nextest run --build-jobs 5
assert_file_contains "${CURRENT_OUT}" 'cargo nextest --build-jobs 5 exceeds selected cap 4'

begin_case
expect_fail cargo nextest run --build-jobs --
assert_file_contains "${CURRENT_OUT}" 'requires a positive integer value'

begin_case
export CARGO_GUARD_TEST_THREADS_MAX=4
expect_fail cargo nextest run --test-threads --
assert_file_contains "${CURRENT_OUT}" 'requires a positive integer runtime thread value'

begin_case
export CARGO_GUARD_TEST_THREADS_MAX=4
expect_fail cargo nextest run --jobs --
assert_file_contains "${CURRENT_OUT}" 'requires a positive integer runtime thread value'

begin_case
export CARGO_GUARD_TEST_THREADS_MAX=4
expect_ok cargo nextest run -- --test-threads 99
assert_file_contains "${CURRENT_LOG}" 'args=nextest run --build-jobs 4 --test-threads 4 -- --test-threads 99 '

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=workspace_nextest
export CARGO_GUARD_EXPECTED_GROWTH_GIB=64
export FAKE_DF_TOTAL_GIB=200
export FAKE_DF_DEFAULT_GIB=120
expect_ok cargo nextest run
assert_file_contains "${CURRENT_OUT}" 'low-disk-clamp=1'
assert_file_contains "${CURRENT_LOG}" 'args=nextest run --build-jobs 4 --test-threads 1 '

begin_case
export CARGO_GUARD_RESOURCE_PROFILE=workspace_nextest
export CARGO_GUARD_EXPECTED_GROWTH_GIB=64
export FAKE_DF_TOTAL_GIB=200
export FAKE_DF_DEFAULT_GIB=190
expect_ok cargo nextest run
assert_file_contains "${CURRENT_OUT}" 'low-disk-clamp=0'
assert_file_contains "${CURRENT_LOG}" 'args=nextest run --build-jobs 4 --test-threads 1 '

begin_case
expect_fail cargo nextest run --test-threads num-cpus
assert_file_contains "${CURRENT_OUT}" 'must be a positive integer'

begin_case
expect_fail cargo nextest run --test-threads -2
assert_file_contains "${CURRENT_OUT}" 'must be a positive integer'

begin_case
expect_fail cargo nextest run --test-threads 2 --jobs 2
assert_file_contains "${CURRENT_OUT}" 'runtime thread count was specified more than once'

begin_case
expect_fail cargo nextest run -j8 --test-threads=8
assert_file_contains "${CURRENT_OUT}" 'runtime thread count was specified more than once'

begin_case
expect_fail cargo nextest run -j8 --jobs=8
assert_file_contains "${CURRENT_OUT}" 'runtime thread count was specified more than once'

begin_case
expect_fail cargo nextest run --jobs=8 --test-threads=4
assert_file_contains "${CURRENT_OUT}" 'runtime thread count was specified more than once'

begin_case
expect_fail cargo nextest run --build-jobs 2 --build-jobs 2
assert_file_contains "${CURRENT_OUT}" 'build-job count was specified more than once'

begin_case
export FAKE_PS_FORCE_BAD_PGID=1
export CARGO_GUARD_TERM_GRACE_SECS=1
expect_fail cargo check -p codex-core
assert_file_contains "${CURRENT_OUT}" 'failed to verify isolated child process group'
assert_file_not_contains "${CURRENT_LOG}" 'args=check -p codex-core '

begin_case
sequence_file="${TMP_ROOT}/self-pgid-sequence"
printf '77777\n77777\n' >"${sequence_file}"
export FAKE_PS_PGID_SEQUENCE_FILE="${sequence_file}"
export CARGO_GUARD_TERM_GRACE_SECS=1
expect_fail cargo check -p codex-core
assert_file_contains "${CURRENT_OUT}" 'failed to verify isolated child process group'
assert_file_not_contains "${CURRENT_LOG}" 'args=check -p codex-core '
assert_file_not_contains "${CURRENT_OUT}" 'terminating guarded Cargo process group'

begin_case
setsid sleep 30 &
external_pid="$!"
EXTRA_PIDS+=("${external_pid}")
external_pgid="$(/usr/bin/ps -o pgid= -p "${external_pid}" | tr -d '[:space:]')"
sequence_file="${TMP_ROOT}/external-pgid-sequence"
printf '%s\n0\n' "${external_pgid}" >"${sequence_file}"
export FAKE_PS_PGID_SEQUENCE_FILE="${sequence_file}"
export CARGO_GUARD_TERM_GRACE_SECS=1
expect_fail cargo check -p codex-core
assert_file_contains "${CURRENT_OUT}" 'failed to verify isolated child process group'
assert_file_not_contains "${CURRENT_OUT}" 'terminating guarded Cargo process group'
assert_file_not_contains "${CURRENT_LOG}" 'args=check -p codex-core '
if ! kill -0 "${external_pid}" 2>/dev/null; then
    fail "unrelated process group was killed during verification failure"
fi
kill -KILL "${external_pid}" 2>/dev/null || true
wait "${external_pid}" 2>/dev/null || true

begin_case
sequence_file="${TMP_ROOT}/missing-pgid-sequence"
printf '99999\n0\n' >"${sequence_file}"
export FAKE_PS_PGID_SEQUENCE_FILE="${sequence_file}"
export CARGO_GUARD_TERM_GRACE_SECS=1
expect_fail cargo check -p codex-core
assert_file_contains "${CURRENT_OUT}" 'failed to verify isolated child process group'
assert_file_not_contains "${CURRENT_OUT}" 'terminating guarded Cargo process group'
assert_file_not_contains "${CURRENT_LOG}" 'args=check -p codex-core '

begin_case
descendant_file="${TMP_ROOT}/descendant.pid"
export FAKE_CARGO_DESCENDANT_FILE="${descendant_file}"
export CARGO_GUARD_TERM_GRACE_SECS=1
./scripts/cargo-guard.sh cargo check -p codex-core >"${CURRENT_OUT}" 2>&1 &
guard_pid="$!"
EXTRA_PIDS+=("${guard_pid}")
wait_for_file "${descendant_file}"
descendant_pid="$(cat "${descendant_file}")"
EXTRA_PIDS+=("${descendant_pid}")
sleep 30 &
unrelated_pid="$!"
EXTRA_PIDS+=("${unrelated_pid}")
kill -TERM "${guard_pid}" 2>/dev/null || true
set +e
wait "${guard_pid}"
guard_status=$?
set -e
if [[ "${guard_status}" == "0" ]]; then
    fail "expected signaled guard to fail"
fi
for _attempt in $(seq 1 20); do
    if ! kill -0 "${descendant_pid}" 2>/dev/null; then
        break
    fi
    sleep 0.1
done
if kill -0 "${descendant_pid}" 2>/dev/null; then
    kill -KILL "${descendant_pid}" 2>/dev/null || true
    fail "expected descendant in guarded process group to be killed"
fi
if ! kill -0 "${unrelated_pid}" 2>/dev/null; then
    fail "unrelated process was killed"
fi
kill -KILL "${unrelated_pid}" 2>/dev/null || true
wait "${unrelated_pid}" 2>/dev/null || true
assert_file_contains "${CURRENT_OUT}" 'terminating guarded Cargo process group'

begin_case
export CARGO_GUARD_MONITOR=1
export CARGO_GUARD_MONITOR_INTERVAL_SECS=1
export CARGO_GUARD_TERM_GRACE_SECS=1
export FAKE_CARGO_COMMAND_SLEEP=3
set_df_sequence 99 99 99 99 99 1 99 99 99 99 99 99
expect_fail cargo check -p codex-core
assert_clean_count 1
assert_package_clean_count codex-core 1
assert_file_contains "${CURRENT_OUT}" 'free space fell below emergency abort threshold during guarded Cargo command'
assert_file_contains "${CURRENT_OUT}" 'guarded Cargo command ended after disk emergency'
python3 - "${CURRENT_HISTORY}" <<'PY'
import json
import sys
from pathlib import Path

entries = [json.loads(line) for line in Path(sys.argv[1]).read_text().splitlines()]
assert len(entries) == 1
entry = entries[0]
assert entry["risk_kind"] == "disk_emergency"
assert entry["disk_emergency"] is True
assert entry["status"] == 70
assert entry["observed_growth_gib"] > 0
PY

begin_case
export CARGO_GUARD_MONITOR=1
export CARGO_GUARD_NO_POST_CLEAN=1
export CARGO_GUARD_MONITOR_INTERVAL_SECS=1
export CARGO_GUARD_TERM_GRACE_SECS=1
export FAKE_CARGO_COMMAND_SLEEP=3
set_df_sequence 99 99 99 99 99 1 99 99
expect_fail cargo check -p codex-core
assert_clean_count 0
assert_file_contains "${CURRENT_OUT}" 'CARGO_GUARD_NO_POST_CLEAN=1 forbids cargo clean \(failed package artifacts after disk emergency\)'

printf '[test-cargo-guard] all tests passed\n'
