#!/usr/bin/env bash
set -euo pipefail

# Merge-safety anchor: workspace bootstrap must keep Cargo/Rustup user-owned and keep Rust build
# artifacts isolated per workspace so `cargo clean` in one repo cannot wipe unrelated repos.

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
CODEX_RS_DIR="${REPO_ROOT}/codex-rs"
CODEX_RS_CONFIG="${CODEX_RS_DIR}/.cargo/config.toml"

UPDATE_TOOLCHAIN=0
INSTALL_COMMON_TOOLS=0
TARGET_USER="${SUDO_USER:-${USER:-$(id -un)}}"

usage() {
    cat <<'EOF'
Usage:
  ./scripts/setup-centralized-rust-cargo.sh [options]

Options:
  --target-user <user>      Override the user account that owns Cargo/Rustup state.
  --update-toolchain        Run `rustup self update` and `rustup update` as the target user.
  --install-common-tools    Refresh common cargo-installed workspace tools.
  --help                    Show this help.

Behavior:
  - Keeps Cargo/Rustup global state under the target user's home.
  - Ensures shell init files source ~/.cargo/env if they do not already.
  - Upserts a per-workspace target-dir for codex-rs under ~/.cache/cargo-target/codex-rs.
  - Safe to invoke with sudo; it targets SUDO_USER by default instead of root.
EOF
}

log() {
    local level="$1"
    shift
    printf '[%s] %s\n' "${level}" "$*"
}

while (($# > 0)); do
    case "$1" in
        --target-user)
            shift
            if (($# == 0)); then
                log error "--target-user requires a value"
                exit 1
            fi
            TARGET_USER="$1"
            ;;
        --update-toolchain)
            UPDATE_TOOLCHAIN=1
            ;;
        --install-common-tools)
            INSTALL_COMMON_TOOLS=1
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            log error "unknown argument: $1"
            usage
            exit 1
            ;;
    esac
    shift
done

if [[ ! -d "${CODEX_RS_DIR}" ]]; then
    log error "expected codex-rs at ${CODEX_RS_DIR}"
    exit 1
fi

if ! id "${TARGET_USER}" >/dev/null 2>&1; then
    log error "target user does not exist: ${TARGET_USER}"
    exit 1
fi

if [[ "${TARGET_USER}" == "root" ]]; then
    log error "refusing to centralize Cargo/Rustup for root; use a real target user"
    exit 1
fi

TARGET_HOME="$(getent passwd "${TARGET_USER}" | cut -d: -f6)"
TARGET_GROUP="$(id -gn "${TARGET_USER}")"
CARGO_HOME_DIR="${TARGET_HOME}/.cargo"
RUSTUP_HOME_DIR="${TARGET_HOME}/.rustup"
WORKSPACE_TARGET_DIR="${TARGET_HOME}/.cache/cargo-target/codex-rs"

if [[ -z "${TARGET_HOME}" || ! -d "${TARGET_HOME}" ]]; then
    log error "could not resolve a valid home directory for ${TARGET_USER}"
    exit 1
fi

run_as_target() {
    local -a env_prefix
    env_prefix=(
        env
        "HOME=${TARGET_HOME}"
        "USER=${TARGET_USER}"
        "LOGNAME=${TARGET_USER}"
        "CARGO_HOME=${CARGO_HOME_DIR}"
        "RUSTUP_HOME=${RUSTUP_HOME_DIR}"
        "PATH=${CARGO_HOME_DIR}/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
    )

    if [[ "$(id -un)" == "${TARGET_USER}" ]]; then
        "${env_prefix[@]}" "$@"
        return
    fi

    sudo -u "${TARGET_USER}" "${env_prefix[@]}" "$@"
}

ensure_user_dir() {
    local directory="$1"
    if [[ "$(id -u)" -eq 0 ]]; then
        install -d -m 0755 -o "${TARGET_USER}" -g "${TARGET_GROUP}" "${directory}"
        return
    fi

    mkdir -p "${directory}"
    chmod 0755 "${directory}"
}

ensure_sourcing_of_cargo_env() {
    local file="$1"
    local source_line='[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"'

    if [[ ! -f "${file}" ]]; then
        if [[ "$(id -u)" -eq 0 ]]; then
            install -m 0644 -o "${TARGET_USER}" -g "${TARGET_GROUP}" /dev/null "${file}"
        else
            install -m 0644 /dev/null "${file}"
        fi
    fi

    if grep -Fq '.cargo/env' "${file}"; then
        log info "${file} already sources ~/.cargo/env"
        return
    fi

    cat >>"${file}" <<EOF

# >>> cargo-centralized >>>
${source_line}
# <<< cargo-centralized <<<
EOF
    if [[ "$(id -u)" -eq 0 ]]; then
        chown "${TARGET_USER}:${TARGET_GROUP}" "${file}"
    fi
    log info "added ~/.cargo/env hook to ${file}"
}

upsert_workspace_target_dir() {
    local config_path="$1"
    local target_dir="$2"
    local tmp_file
    tmp_file="$(mktemp)"

    if [[ ! -f "${config_path}" ]]; then
        if [[ "$(id -u)" -eq 0 ]]; then
            install -d -m 0755 -o "${TARGET_USER}" -g "${TARGET_GROUP}" "$(dirname -- "${config_path}")"
            install -m 0644 -o "${TARGET_USER}" -g "${TARGET_GROUP}" /dev/null "${config_path}"
        else
            install -d -m 0755 "$(dirname -- "${config_path}")"
            install -m 0644 /dev/null "${config_path}"
        fi
    fi

    awk -v target_dir="${target_dir}" '
        BEGIN {
            in_build = 0;
            saw_build = 0;
            inserted = 0;
            replaced = 0;
            last_was_blank = 1;
        }
        /^\[build\][[:space:]]*$/ {
            if (in_build && !inserted && !replaced) {
                print "target-dir = \"" target_dir "\"";
                inserted = 1;
            }
            in_build = 1;
            saw_build = 1;
            print;
            last_was_blank = 0;
            next;
        }
        /^\[/ {
            if (in_build && !inserted && !replaced) {
                print "target-dir = \"" target_dir "\"";
                inserted = 1;
            }
            in_build = 0;
            print;
            last_was_blank = 0;
            next;
        }
        {
            if (in_build && $0 ~ /^[[:space:]]*target-dir[[:space:]]*=/) {
                print "target-dir = \"" target_dir "\"";
                replaced = 1;
                inserted = 1;
                last_was_blank = 0;
                next;
            }
            print;
            last_was_blank = ($0 ~ /^[[:space:]]*$/);
        }
        END {
            if (in_build && !inserted && !replaced) {
                print "target-dir = \"" target_dir "\"";
                last_was_blank = 0;
            }
            if (!saw_build) {
                if (NR > 0 && !last_was_blank) {
                    print "";
                }
                print "[build]";
                print "target-dir = \"" target_dir "\"";
            }
        }
    ' "${config_path}" >"${tmp_file}"

    mv "${tmp_file}" "${config_path}"
    if [[ "$(id -u)" -eq 0 ]]; then
        chown "${TARGET_USER}:${TARGET_GROUP}" "${config_path}"
    fi
    chmod 0644 "${config_path}"
    log info "upserted codex-rs target-dir in ${config_path}"
}

refresh_toolchain() {
    run_as_target rustup self update
    run_as_target rustup update
}

refresh_common_tools() {
    local tool
    local -a tools=(
        cargo-insta
        cargo-nextest
        cargo-update
        just
    )

    for tool in "${tools[@]}"; do
        run_as_target cargo install --locked --force "${tool}"
    done
}

log info "target user: ${TARGET_USER}"
log info "target home: ${TARGET_HOME}"
log info "repo root: ${REPO_ROOT}"

ensure_user_dir "${CARGO_HOME_DIR}"
ensure_user_dir "${RUSTUP_HOME_DIR}"
ensure_user_dir "${TARGET_HOME}/.cache"
ensure_user_dir "${TARGET_HOME}/.cache/cargo-target"
ensure_user_dir "${WORKSPACE_TARGET_DIR}"

ensure_sourcing_of_cargo_env "${TARGET_HOME}/.bashrc"
ensure_sourcing_of_cargo_env "${TARGET_HOME}/.profile"
if [[ -f "${TARGET_HOME}/.zshrc" ]]; then
    ensure_sourcing_of_cargo_env "${TARGET_HOME}/.zshrc"
fi

upsert_workspace_target_dir "${CODEX_RS_CONFIG}" "${WORKSPACE_TARGET_DIR}"

if ((UPDATE_TOOLCHAIN)); then
    log info "refreshing Rust toolchain"
    refresh_toolchain
fi

if ((INSTALL_COMMON_TOOLS)); then
    log info "refreshing common cargo-installed tools"
    refresh_common_tools
fi

cat <<EOF

Done.

Centralized global state:
  CARGO_HOME=${CARGO_HOME_DIR}
  RUSTUP_HOME=${RUSTUP_HOME_DIR}

Per-workspace Rust build cache:
  ${WORKSPACE_TARGET_DIR}

codex-rs Cargo config:
  ${CODEX_RS_CONFIG}

Open a new shell after running this script if your current session has stale PATH state.
EOF
