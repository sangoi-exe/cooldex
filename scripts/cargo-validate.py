#!/usr/bin/env python3
"""Deterministic Cargo prep and validation planner for the Codex workspace."""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import tomllib
from dataclasses import dataclass, field, replace
from fnmatch import fnmatch
from pathlib import Path
from typing import Any, BinaryIO, Iterable

# Merge-safety anchor: cargo-validate owns deterministic mechanical-prep and
# validation planning, receipt placement, and resource-profile expansion; Cargo
# build-like execution stays delegated to scripts/cargo-guard.sh.

VALID_MODES = ("quick", "standard", "strict", "full")
PLAN_ACTIONS = {"plan", "prep-plan"}
PREP_ACTIONS = {"prep", "prep-plan"}
VALIDATION_ACTIONS = {"plan", "verify"}
FIRST_PARTY_RUNTIME_SUPPORT_BINS_COMMAND = "first-party-runtime-support-bins"
FIRST_PARTY_RUNTIME_EXPECTED_GROWTH_SOURCE = "forced:post-support-bins"
FIRST_PARTY_RUNTIME_SUPPORT_PACKAGES = {
    "codex-app-server",
    "codex-core",
    "codex-rmcp-client",
}
BUILD_LIKE_CARGO = {
    "bench",
    "build",
    "check",
    "clippy",
    "doc",
    "fix",
    "install",
    "nextest",
    "run",
    "rustc",
    "test",
}
COMMAND_LOG_DIR_NAME = "command-logs"
COMMAND_LOG_READ_CHUNK_BYTES = 64 * 1024
COMMAND_LOG_PIPE_DRAIN_GRACE_SECONDS = 0.5
COMMAND_LOG_POLL_INTERVAL_SECONDS = 0.02
FEATURE_CFG_RE = re.compile(r"\bcfg(?:_attr)?!?\s*\([\s\S]*?\bfeature\s*=")
DIFF_HUNK_RE = re.compile(
    r"^@@ -(?P<old_start>\d+)(?:,(?P<old_count>\d+))? \+(?P<new_start>\d+)(?:,(?P<new_count>\d+))? @@"
)
RESOURCE_PROFILE_ENV_KEYS = {
    "reserve_free_pct": "CARGO_GUARD_RESERVE_FREE_PCT",
    "reserve_free_gib": "CARGO_GUARD_RESERVE_FREE_GIB",
    "abort_free_pct": "CARGO_GUARD_ABORT_FREE_PCT",
    "abort_free_gib": "CARGO_GUARD_ABORT_FREE_GIB",
    "monitor": "CARGO_GUARD_MONITOR",
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
LEGACY_OR_ALIAS_PROFILE_KEYS = {
    "jobs_mode",
    "jobs_min",
    "jobs_max",
    "jobs_hard_max",
    "low_disk_jobs_max",
    "mem_per_job_gib",
    "mem_reserve_gib",
    "expected_growth_gib",
}
RESUME_IDENTITY_IGNORED_ENV_KEYS = {
    "CARGO_GUARD_EXPECTED_GROWTH_GIB",
    "CARGO_GUARD_METRICS_PATH",
    "CARGO_GUARD_TELEMETRY_PATH",
}
TELEMETRY_LEVELS = ("off", "summary", "full", "debug")
DEFAULT_TELEMETRY_LEVEL = "full"

GUARDED_JUST_RECIPES = {
    "build-codex-bin",
    "check-codex-bin",
    "check-strict",
    "clippy",
    "clippy-fix",
    "clippy-strict",
    "mcp-server-run",
    "smoke-codex-bin",
    "strict-codex-bin",
    "test",
    "write-app-server-schema",
    "write-config-schema",
    "write-hooks-schema",
}
VALIDATION_TOOLING_PATHS = (
    "scripts/cargo-validate.py",
    "scripts/cargo-guard.sh",
    "scripts/cargo-validation.toml",
    "justfile",
    "codex-rs/.config/nextest.toml",
)


class PlannerError(Exception):
    """Raised when validation planning must fail closed."""


class CommandOutputError(PlannerError):
    """Raised after a command ran but its output could not be recorded fully."""

    def __init__(self, message: str, return_code: int) -> None:
        super().__init__(message)
        self.return_code = return_code


@dataclass(frozen=True)
class CommandEntry:
    argv: tuple[str, ...]
    reason: str
    kind: str = "command"
    env: dict[str, str] = field(default_factory=dict)
    resource_profile: str | None = None
    fingerprint: str | None = None
    job_contract_digest: str | None = None
    fallback_expected_growth_gib: int | None = None
    effective_expected_growth_gib: int | None = None
    expected_growth_source: str | None = None

    def to_json(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "argv": list(self.argv),
            "reason": self.reason,
            "kind": self.kind,
            "env": dict(sorted(self.env.items())),
            "command_id": command_resume_id(self),
        }
        if self.resource_profile is not None:
            payload["resource_profile"] = self.resource_profile
        if self.fingerprint is not None:
            payload["fingerprint"] = self.fingerprint
        if self.job_contract_digest is not None:
            payload["job_contract_digest"] = self.job_contract_digest
        if self.fallback_expected_growth_gib is not None:
            payload["fallback_expected_growth_gib"] = self.fallback_expected_growth_gib
        if self.effective_expected_growth_gib is not None:
            payload["effective_expected_growth_gib"] = (
                self.effective_expected_growth_gib
            )
        if self.expected_growth_source is not None:
            payload["expected_growth_source"] = self.expected_growth_source
        return payload


@dataclass(frozen=True)
class ManualEntry:
    message: str
    reason: str
    kind: str = "manual"

    def to_json(self) -> dict[str, Any]:
        return {"message": self.message, "reason": self.reason, "kind": self.kind}


@dataclass
class PackageInfo:
    name: str
    manifest_path: Path
    root_path: Path
    has_test_targets: bool
    has_doctests: bool


@dataclass(frozen=True)
class RevisionPath:
    path: str
    deleted: bool


@dataclass
class PathSelectionEvidence:
    saw_revision_deleted: bool = False
    saw_revision_non_deleted: bool = False
    saw_explicit_file: bool = False


@dataclass
class Selection:
    files: list[str]
    path_evidence: dict[str, PathSelectionEvidence] = field(default_factory=dict)
    packages: dict[str, set[str]] = field(default_factory=dict)
    surfaces: dict[str, set[str]] = field(default_factory=dict)
    flags: set[str] = field(default_factory=set)
    generators: dict[str, set[str]] = field(default_factory=dict)
    prep_command_names: dict[str, set[str]] = field(default_factory=dict)
    prep_command_order: list[str] = field(default_factory=list)
    command_names: dict[str, set[str]] = field(default_factory=dict)
    command_order: list[str] = field(default_factory=list)
    feature_errors: list[str] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)
    unknown_path_fallbacks: list[str] = field(default_factory=list)

    def add_package(self, package: str, reason: str) -> None:
        self.packages.setdefault(package, set()).add(reason)

    def add_surface(self, surface: str, reason: str) -> None:
        self.surfaces.setdefault(surface, set()).add(reason)

    def add_generator(self, generator: str, reason: str) -> None:
        self.generators.setdefault(generator, set()).add(reason)

    def add_prep_command_name(self, command_name: str, reason: str) -> None:
        if command_name not in self.prep_command_names:
            self.prep_command_order.append(command_name)
        self.prep_command_names.setdefault(command_name, set()).add(reason)

    def add_command_name(self, command_name: str, reason: str) -> None:
        if command_name not in self.command_names:
            self.command_order.append(command_name)
        self.command_names.setdefault(command_name, set()).add(reason)

    def add_unknown_path_fallback(self, file_path: str, package: str) -> None:
        message = f"unmapped Rust package path used CLI fallback: {file_path} belongs to package {package}"
        self.unknown_path_fallbacks.append(message)
        self.warnings.append(message)


@dataclass
class Plan:
    action: str
    stage: str
    mode: str
    files: list[str]
    selected_packages: list[str]
    selected_surfaces: list[str]
    flags: list[str]
    warnings: list[str]
    commands: list[CommandEntry]
    manual: list[ManualEntry]
    receipt_dir: Path | None
    telemetry_level: str

    def to_json(self) -> dict[str, Any]:
        return {
            "action": self.action,
            "stage": self.stage,
            "mode": self.mode,
            "changed_files": self.files,
            "selected_packages": self.selected_packages,
            "selected_surfaces": self.selected_surfaces,
            "flags": self.flags,
            "warnings": self.warnings,
            "commands": [command.to_json() for command in self.commands],
            "manual": [entry.to_json() for entry in self.manual],
            "receipt_dir": str(self.receipt_dir) if self.receipt_dir else None,
            "telemetry_level": self.telemetry_level,
        }


def repo_root_from_script() -> Path:
    return Path(__file__).resolve().parents[1]


def normalize_repo_path(path_text: str, repo_root: Path) -> str:
    raw_path = Path(path_text)
    if raw_path.is_absolute():
        try:
            return raw_path.resolve().relative_to(repo_root).as_posix()
        except ValueError:
            return raw_path.as_posix()
    return raw_path.as_posix().lstrip("./")


def run_capture(argv: list[str], cwd: Path) -> str:
    process = subprocess.run(
        argv,
        cwd=cwd,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if process.returncode != 0:
        stderr = process.stderr.strip()
        raise PlannerError(
            f"command failed ({process.returncode}): {' '.join(argv)}\n{stderr}"
        )
    return process.stdout


def load_config(config_path: Path) -> dict[str, Any]:
    try:
        return tomllib.loads(config_path.read_text())
    except tomllib.TOMLDecodeError as error:
        raise PlannerError(f"failed to parse {config_path}: {error}") from error


def git_changed_files(repo_root: Path) -> list[str]:
    commands = [
        ["git", "diff", "--name-only", "--cached"],
        ["git", "diff", "--name-only"],
        ["git", "ls-files", "--others", "--exclude-standard"],
    ]
    seen: set[str] = set()
    files: list[str] = []
    for command in commands:
        output = run_capture(command, repo_root)
        for line in output.splitlines():
            normalized = normalize_repo_path(line, repo_root)
            if normalized and normalized not in seen:
                seen.add(normalized)
                files.append(normalized)
    return files


def git_revision_parents(repo_root: Path, revision: str) -> list[str]:
    output = run_capture(
        ["git", "rev-list", "--parents", "-n", "1", revision], repo_root
    )
    parts = output.strip().split()
    if not parts:
        raise PlannerError(f"revision {revision!r} did not resolve to a commit")
    return parts[1:]


def parse_revision_name_status(output: str, repo_root: Path) -> list[RevisionPath]:
    selections: list[RevisionPath] = []
    for line in output.splitlines():
        if not line:
            continue
        fields = line.split("\t")
        status = fields[0]
        if not status:
            raise PlannerError(f"empty Git name-status record: {line!r}")
        status_kind = status[0]
        if status_kind in {"R", "C"}:
            if len(status) == 1 or not status[1:].isdigit() or len(fields) != 3:
                raise PlannerError(f"malformed Git name-status record: {line!r}")
            path_text = fields[2]
        elif status in {"A", "D", "M", "T"}:
            if len(fields) != 2:
                raise PlannerError(f"malformed Git name-status record: {line!r}")
            path_text = fields[1]
        else:
            raise PlannerError(f"unsupported Git name-status record: {line!r}")
        normalized = normalize_repo_path(path_text, repo_root)
        if not normalized:
            raise PlannerError(f"empty path in Git name-status record: {line!r}")
        selections.append(RevisionPath(path=normalized, deleted=status_kind == "D"))
    return selections


def git_commit_files(repo_root: Path, revision: str) -> list[RevisionPath]:
    parents = git_revision_parents(repo_root, revision)
    if len(parents) > 1:
        raise PlannerError(
            f"--commit {revision!r} resolves to a merge commit; use --range <base>..{revision}"
        )
    output = run_capture(
        [
            "git",
            "diff-tree",
            "--no-commit-id",
            "--name-status",
            "-r",
            "--root",
            revision,
        ],
        repo_root,
    )
    return parse_revision_name_status(output, repo_root)


def git_range_files(repo_root: Path, revision_range: str) -> list[RevisionPath]:
    output = run_capture(["git", "diff", "--name-status", revision_range], repo_root)
    return parse_revision_name_status(output, repo_root)


def add_selected_files(
    files: list[str],
    seen: set[str],
    file_paths: Iterable[str],
    repo_root: Path,
    *,
    path_evidence: dict[str, PathSelectionEvidence] | None = None,
    mark_explicit: bool = False,
) -> None:
    for file_path in file_paths:
        normalized = normalize_repo_path(file_path, repo_root)
        if normalized and mark_explicit:
            if path_evidence is None:
                raise PlannerError("explicit selector evidence storage is missing")
            path_evidence.setdefault(
                normalized, PathSelectionEvidence()
            ).saw_explicit_file = True
        if normalized and normalized not in seen:
            seen.add(normalized)
            files.append(normalized)


def add_revision_files(
    files: list[str],
    seen: set[str],
    revision_paths: Iterable[RevisionPath],
    path_evidence: dict[str, PathSelectionEvidence],
) -> None:
    for revision_path in revision_paths:
        evidence = path_evidence.setdefault(revision_path.path, PathSelectionEvidence())
        if revision_path.deleted:
            evidence.saw_revision_deleted = True
        else:
            evidence.saw_revision_non_deleted = True
        if revision_path.path not in seen:
            seen.add(revision_path.path)
            files.append(revision_path.path)


def load_metadata(repo_root: Path, metadata_json: Path | None) -> list[PackageInfo]:
    if metadata_json:
        metadata = json.loads(metadata_json.read_text())
    else:
        metadata_output = run_capture(
            ["cargo", "metadata", "--format-version=1", "--no-deps", "--quiet"],
            repo_root / "codex-rs",
        )
        metadata = json.loads(metadata_output)

    if not isinstance(metadata, dict) or not isinstance(metadata.get("packages"), list):
        raise PlannerError("cargo metadata packages must be a list")

    packages: list[PackageInfo] = []
    for package_index, package in enumerate(metadata["packages"]):
        if not isinstance(package, dict):
            raise PlannerError(
                f"cargo metadata package {package_index} must be an object"
            )
        package_name = package.get("name")
        if not isinstance(package_name, str) or not package_name:
            raise PlannerError(
                f"cargo metadata package {package_index} name must be a non-empty string"
            )
        manifest_path_value = package.get("manifest_path")
        if not isinstance(manifest_path_value, str) or not manifest_path_value:
            raise PlannerError(
                f"cargo metadata package {package_name!r} manifest_path "
                "must be a non-empty string"
            )
        targets = package.get("targets")
        if not isinstance(targets, list) or not targets:
            raise PlannerError(
                f"cargo metadata package {package_name!r} targets "
                "must be a non-empty list"
            )

        has_test_targets = False
        has_doctests = False
        for target_index, target in enumerate(targets):
            if not isinstance(target, dict):
                raise PlannerError(
                    f"cargo metadata package {package_name!r} target {target_index} "
                    "must be an object"
                )
            test_capability = target.get("test")
            doctest_capability = target.get("doctest")
            if not isinstance(test_capability, bool):
                raise PlannerError(
                    f"cargo metadata package {package_name!r} target {target_index} "
                    "field 'test' must be boolean"
                )
            if not isinstance(doctest_capability, bool):
                raise PlannerError(
                    f"cargo metadata package {package_name!r} target {target_index} "
                    "field 'doctest' must be boolean"
                )
            has_test_targets |= test_capability
            has_doctests |= doctest_capability

        manifest_path = Path(manifest_path_value).resolve()
        packages.append(
            PackageInfo(
                name=package_name,
                manifest_path=manifest_path,
                root_path=manifest_path.parent,
                has_test_targets=has_test_targets,
                has_doctests=has_doctests,
            )
        )
    packages.sort(key=lambda package: len(package.root_path.as_posix()), reverse=True)
    return packages


def package_for_file(
    file_path: str, repo_root: Path, packages: list[PackageInfo]
) -> str | None:
    absolute_path = (repo_root / file_path).resolve()
    for package in packages:
        try:
            absolute_path.relative_to(package.root_path)
        except ValueError:
            continue
        return package.name
    return None


def is_git_deleted_path(repo_root: Path, file_path: str) -> bool:
    if not (repo_root / ".git").exists():
        return False
    for command in (
        ["git", "diff", "--cached", "--name-status", "--", file_path],
        ["git", "diff", "--name-status", "--", file_path],
    ):
        process = subprocess.run(
            command,
            cwd=repo_root,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if process.returncode not in (0, 1):
            continue
        for line in process.stdout.splitlines():
            status = line.split("\t", 1)[0]
            if status == "D":
                return True
    return False


def read_file_text(repo_root: Path, file_path: str) -> str:
    absolute_path = repo_root / file_path
    if not absolute_path.is_file():
        return ""
    try:
        return absolute_path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return ""


def git_changed_lines(repo_root: Path, file_path: str) -> list[str] | None:
    if not (repo_root / ".git").exists():
        return None
    changed: list[str] = []
    saw_diff = False
    for command in (
        ["git", "diff", "--cached", "-U0", "--", file_path],
        ["git", "diff", "-U0", "--", file_path],
    ):
        process = subprocess.run(
            command,
            cwd=repo_root,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if process.returncode not in (0, 1):
            continue
        if process.stdout.strip():
            saw_diff = True
        for line in process.stdout.splitlines():
            if line.startswith("+++"):
                continue
            if line.startswith("---"):
                continue
            if line.startswith("+"):
                changed.append(line[1:])
            if line.startswith("-"):
                changed.append(line[1:])
    return changed if saw_diff else None


def span_from_hunk(start_text: str, count_text: str | None) -> tuple[int, int] | None:
    start = int(start_text)
    count = int(count_text) if count_text is not None else 1
    if count <= 0:
        return None
    return start, start + count - 1


def git_changed_spans(
    repo_root: Path, file_path: str
) -> tuple[list[tuple[int, int]], list[tuple[int, int]]] | None:
    if not (repo_root / ".git").exists():
        return None
    old_spans: list[tuple[int, int]] = []
    new_spans: list[tuple[int, int]] = []
    saw_diff = False
    for command in (
        ["git", "diff", "--cached", "-U0", "--", file_path],
        ["git", "diff", "-U0", "--", file_path],
    ):
        process = subprocess.run(
            command,
            cwd=repo_root,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if process.returncode not in (0, 1):
            continue
        if process.stdout.strip():
            saw_diff = True
        for line in process.stdout.splitlines():
            match = DIFF_HUNK_RE.match(line)
            if not match:
                continue
            old_span = span_from_hunk(
                match.group("old_start"), match.group("old_count")
            )
            new_span = span_from_hunk(
                match.group("new_start"), match.group("new_count")
            )
            if old_span is not None:
                old_spans.append(old_span)
            if new_span is not None:
                new_spans.append(new_span)
    return (old_spans, new_spans) if saw_diff else None


def git_show_head_text(repo_root: Path, file_path: str) -> str | None:
    if not (repo_root / ".git").exists():
        return None
    process = subprocess.run(
        ["git", "show", f"HEAD:{file_path}"],
        cwd=repo_root,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if process.returncode != 0:
        return None
    return process.stdout


def feature_scan_text(repo_root: Path, file_path: str) -> tuple[str, bool]:
    changed_lines = git_changed_lines(repo_root, file_path)
    if changed_lines is not None:
        return "\n".join(changed_lines), True
    return read_file_text(repo_root, file_path), False


def spans_intersect(left: tuple[int, int], right: tuple[int, int]) -> bool:
    return left[0] <= right[1] and right[0] <= left[1]


def rust_attribute_end_index(lines: list[str], start_index: int) -> int:
    balance = 0
    for index in range(start_index, len(lines)):
        balance += lines[index].count("[")
        balance -= lines[index].count("]")
        if balance <= 0 and "]" in lines[index]:
            return index
    return start_index


def feature_cfg_regions(text: str) -> list[tuple[int, int]]:
    lines = text.splitlines()
    regions: list[tuple[int, int]] = []
    index = 0
    while index < len(lines):
        line = lines[index]
        stripped = line.strip()
        attribute_end = index
        candidate_text = line
        if stripped.startswith("#["):
            attribute_end = rust_attribute_end_index(lines, index)
            candidate_text = "\n".join(lines[index : attribute_end + 1])
        if not FEATURE_CFG_RE.search(candidate_text):
            index += 1
            continue
        start_line = index + 1
        item_index = attribute_end + 1
        while item_index < len(lines):
            stripped = lines[item_index].strip()
            if not stripped:
                item_index += 1
                continue
            if stripped.startswith("#["):
                item_index = rust_attribute_end_index(lines, item_index) + 1
                continue
            break
        if item_index >= len(lines):
            regions.append((start_line, start_line))
            index += 1
            continue
        end_line = item_index + 1
        brace_balance = 0
        saw_open = False
        for scan_index in range(item_index, len(lines)):
            scan_line = lines[scan_index]
            brace_balance += scan_line.count("{")
            if "{" in scan_line:
                saw_open = True
            brace_balance -= scan_line.count("}")
            end_line = scan_index + 1
            if saw_open and brace_balance <= 0:
                break
            if not saw_open and ";" in scan_line:
                break
        regions.append((start_line, end_line))
        index += 1
    return regions


def contains_feature_cfg(text: str) -> bool:
    return bool(feature_cfg_regions(text))


def feature_regions_intersect_changed_spans(
    text: str, changed_spans: list[tuple[int, int]]
) -> bool:
    if not changed_spans:
        return False
    regions = feature_cfg_regions(text)
    for changed_span in changed_spans:
        if any(spans_intersect(changed_span, region) for region in regions):
            return True
    return False


def changed_hunk_touches_feature_region(repo_root: Path, file_path: str) -> bool:
    changed_spans = git_changed_spans(repo_root, file_path)
    if changed_spans is None:
        return False
    old_spans, new_spans = changed_spans
    if feature_regions_intersect_changed_spans(
        read_file_text(repo_root, file_path), new_spans
    ):
        return True
    old_text = git_show_head_text(repo_root, file_path)
    return old_text is not None and feature_regions_intersect_changed_spans(
        old_text, old_spans
    )


def package_has_feature_profile(config: dict[str, Any], package: str | None) -> bool:
    if not package:
        return False
    for entry in config.get("package_features", []):
        if entry.get("package") == package:
            return True
    return False


def profile_config(
    config: dict[str, Any], profile_name: str | None
) -> dict[str, Any] | None:
    if not profile_name:
        return None
    profile = config.get("resource_profiles", {}).get(profile_name)
    if not isinstance(profile, dict):
        raise PlannerError(
            f"resource profile {profile_name!r} is not defined in cargo-validation.toml"
        )
    return profile


def profile_env(
    config: dict[str, Any],
    profile_name: str | None,
    *,
    expected_growth_override: int | None = None,
) -> dict[str, str]:
    profile = profile_config(config, profile_name)
    if not profile_name or profile is None:
        return {}
    env = {"CARGO_GUARD_RESOURCE_PROFILE": profile_name}
    for key, env_key in RESOURCE_PROFILE_ENV_KEYS.items():
        if key not in profile:
            continue
        value = profile[key]
        if isinstance(value, bool):
            env[env_key] = "1" if value else "0"
        else:
            env[env_key] = str(value)
    if expected_growth_override is not None:
        env["CARGO_GUARD_EXPECTED_GROWTH_GIB"] = str(expected_growth_override)
    return env


def command_fingerprint(
    profile_name: str | None, job_contract_digest: str | None, argv: tuple[str, ...]
) -> str:
    payload = json.dumps(
        {
            "schema": 2,
            "resource_profile": profile_name,
            "job_contract_digest": job_contract_digest,
            "argv": list(argv),
        },
        sort_keys=True,
        separators=(",", ":"),
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def stable_digest(payload: Any) -> str:
    return hashlib.sha256(
        json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()


def profile_job_contract_digest(
    config: dict[str, Any], profile_name: str | None
) -> str | None:
    if profile_name is None:
        return None
    profile = profile_config(config, profile_name)
    if profile is None:
        return None
    jobs_max = int(profile.get("cargo_jobs_max", 4))
    return stable_digest(
        {
            "schema": 1,
            "resource_profile": profile_name,
            "jobs_mode": profile.get("cargo_jobs_mode", "fixed"),
            "jobs_default": profile.get("cargo_jobs_default", "min"),
            "jobs_min": int(profile.get("cargo_jobs_min", 4)),
            "jobs_max": jobs_max,
            "jobs_hard_max": int(profile.get("cargo_jobs_hard_max", jobs_max)),
            "jobs_low_disk_max": int(profile.get("cargo_jobs_low_disk_max", jobs_max)),
            "jobs_cpu_pct": int(profile.get("cargo_jobs_cpu_pct", 100)),
            "jobs_cpu_reserve": int(profile.get("cargo_jobs_cpu_reserve", 0)),
            "jobs_mem_per_job_mib": int(profile.get("cargo_jobs_mem_per_job_mib", 1)),
            "jobs_mem_reserve_mib": int(profile.get("cargo_jobs_mem_reserve_mib", 0)),
        }
    )


def file_digest_record(repo_root: Path, file_path: str) -> dict[str, Any]:
    absolute_path = repo_root / file_path
    record: dict[str, Any] = {"path": file_path}
    if absolute_path.is_file():
        data = absolute_path.read_bytes()
        record.update(
            {
                "kind": "file",
                "size": len(data),
                "sha256": hashlib.sha256(data).hexdigest(),
            }
        )
    elif absolute_path.exists():
        record.update({"kind": "non-file"})
    else:
        record.update({"kind": "missing"})
    return record


def git_head_digest(repo_root: Path) -> str:
    process = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=repo_root,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    if process.returncode != 0:
        return "not-a-git-repo"
    return process.stdout.strip()


def validation_tooling_digest(repo_root: Path) -> str:
    return stable_digest(
        {
            "schema": 1,
            "files": [
                file_digest_record(repo_root, file_path)
                for file_path in VALIDATION_TOOLING_PATHS
            ],
        }
    )


def resume_identity_env(env: dict[str, str]) -> dict[str, str]:
    return {
        key: value
        for key, value in env.items()
        if key not in RESUME_IDENTITY_IGNORED_ENV_KEYS
    }


def command_resume_id(command: CommandEntry) -> str:
    return stable_digest(
        {
            "schema": 1,
            "argv": list(command.argv),
            "env": dict(sorted(resume_identity_env(command.env).items())),
            "kind": command.kind,
            "resource_profile": command.resource_profile,
            "fingerprint": command.fingerprint,
            "job_contract_digest": command.job_contract_digest,
        }
    )


def command_resume_key(index: int, command: CommandEntry) -> str:
    return stable_digest(
        {"schema": 1, "index": index, "command_id": command_resume_id(command)}
    )


def plan_resume_id(plan: Plan, tooling_digest: str) -> str:
    return stable_digest(
        {
            "schema": 2,
            "action": plan.action,
            "stage": plan.stage,
            "mode": plan.mode,
            "changed_files": sorted(plan.files),
            "selected_packages": sorted(plan.selected_packages),
            "selected_surfaces": sorted(plan.selected_surfaces),
            "flags": sorted(plan.flags),
            "telemetry_level": plan.telemetry_level,
            "validation_tooling_digest": tooling_digest,
            "commands": [
                {"index": index, "command_key": command_resume_key(index, command)}
                for index, command in enumerate(plan.commands, start=1)
            ],
        }
    )


def plan_input_digest(plan: Plan, repo_root: Path) -> str:
    return stable_digest(
        {
            "schema": 1,
            "head": git_head_digest(repo_root),
            "changed_files": [
                file_digest_record(repo_root, file_path)
                for file_path in sorted(set(plan.files))
            ],
        }
    )


def default_history_sample_limit(config: dict[str, Any]) -> int:
    value = config.get("defaults", {}).get("history_sample_limit", 20)
    validate_positive_int(value, "defaults.history_sample_limit")
    return int(value)


def reject_stale_history_multiplier_defaults(config: dict[str, Any]) -> None:
    defaults = config.get("defaults", {})
    stale_keys = (
        "history_growth_multiplier_pct",
        "success_history_growth_multiplier_pct",
        "disk_emergency_history_growth_multiplier_pct",
    )
    for key in stale_keys:
        if key in defaults:
            raise PlannerError(f"defaults.{key} uses a stale key name")


def read_history_entries(history_path: Path | None) -> list[dict[str, Any]]:
    if history_path is None or not history_path.is_file():
        return []
    entries: list[dict[str, Any]] = []
    for line in history_path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(entry, dict):
            entries.append(entry)
    return entries


def effective_expected_growth(
    *,
    config: dict[str, Any],
    history_entries: list[dict[str, Any]],
    profile_name: str | None,
    fingerprint: str | None,
    fallback_expected_growth_gib: int | None,
) -> tuple[int | None, str | None]:
    if (
        profile_name is None
        or fingerprint is None
        or fallback_expected_growth_gib is None
    ):
        return fallback_expected_growth_gib, None
    sample_limit = default_history_sample_limit(config)
    matching_success: list[int] = []
    matching_emergency: list[int] = []
    for entry in history_entries:
        if entry.get("resource_profile") != profile_name:
            continue
        if entry.get("fingerprint") != fingerprint:
            continue
        observed = entry.get("observed_growth_gib")
        if not isinstance(observed, int) or isinstance(observed, bool) or observed < 0:
            continue
        risk_kind = entry.get("risk_kind")
        if (
            risk_kind == "success"
            and entry.get("status") == 0
            and entry.get("disk_emergency") is not True
        ):
            matching_success.append(observed)
        elif risk_kind == "disk_emergency" and entry.get("disk_emergency") is True:
            matching_emergency.append(observed)
    if not matching_success and not matching_emergency:
        return fallback_expected_growth_gib, "fallback:no-history"
    candidates = [fallback_expected_growth_gib]
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
        source_parts.append(
            f"disk_emergency:max={history_growth},samples={len(sample)}"
        )
    return max(candidates), "history:" + ";".join(source_parts)


def command_with_profile(
    argv: tuple[str, ...],
    reason: str,
    kind: str,
    config: dict[str, Any],
    profile_name: str | None,
    history_entries: list[dict[str, Any]],
    *,
    use_growth_history: bool = True,
) -> CommandEntry:
    profile = profile_config(config, profile_name)
    job_contract_digest = profile_job_contract_digest(config, profile_name)
    fingerprint = (
        command_fingerprint(profile_name, job_contract_digest, argv)
        if profile_name is not None
        else None
    )
    is_direct_guard = (
        len(argv) >= 3 and argv[0] == "./scripts/cargo-guard.sh" and argv[1] == "cargo"
    )
    fallback_growth = 0 if profile is not None and is_direct_guard else None
    command_history_entries = (
        history_entries if is_direct_guard and use_growth_history else []
    )
    effective_growth, growth_source = effective_expected_growth(
        config=config,
        history_entries=command_history_entries,
        profile_name=profile_name,
        fingerprint=fingerprint,
        fallback_expected_growth_gib=fallback_growth,
    )
    env = profile_env(config, profile_name, expected_growth_override=effective_growth)
    return CommandEntry(
        argv,
        reason,
        kind,
        env,
        resource_profile=profile_name,
        fingerprint=fingerprint,
        job_contract_digest=job_contract_digest,
        fallback_expected_growth_gib=fallback_growth,
        effective_expected_growth_gib=effective_growth,
        expected_growth_source=growth_source,
    )


def validate_positive_int(
    value: Any, context: str, *, allow_zero: bool = False
) -> None:
    if not isinstance(value, int) or isinstance(value, bool):
        raise PlannerError(f"{context} must be an integer")
    if allow_zero:
        if value < 0:
            raise PlannerError(f"{context} must be non-negative")
    elif value <= 0:
        raise PlannerError(f"{context} must be positive")


def raw_build_like_cargo(argv: list[str]) -> bool:
    if not argv or argv[0] != "cargo":
        return False
    for arg in argv[1:]:
        if arg.startswith("-") or arg.startswith("+"):
            continue
        return arg in BUILD_LIKE_CARGO
    return False


def validate_config(config: dict[str, Any], packages: list[PackageInfo]) -> None:
    if config.get("schema_version") != 1:
        raise PlannerError("cargo-validation.toml schema_version must be 1")

    default_history_sample_limit(config)
    reject_stale_history_multiplier_defaults(config)

    package_names = {package.name for package in packages}
    command_names = set(config.get("commands", {}))
    surface_names = {surface.get("name") for surface in config.get("surfaces", [])}
    profile_names = set(config.get("resource_profiles", {}))
    profile_keys = set(RESOURCE_PROFILE_ENV_KEYS)

    for profile_name, profile in config.get("resource_profiles", {}).items():
        if not isinstance(profile, dict):
            raise PlannerError(f"resource profile {profile_name!r} must be a table")
        unknown_keys = set(profile) - profile_keys
        legacy_keys = unknown_keys & LEGACY_OR_ALIAS_PROFILE_KEYS
        if legacy_keys:
            bad_key = sorted(legacy_keys)[0]
            raise PlannerError(
                f"resource profile {profile_name}.{bad_key} uses a stale key name"
            )
        if unknown_keys:
            bad_key = sorted(unknown_keys)[0]
            raise PlannerError(
                f"resource profile {profile_name}.{bad_key} is not a supported key"
            )
        for key in (
            "reserve_free_pct",
            "reserve_free_gib",
            "abort_free_pct",
            "abort_free_gib",
        ):
            if key in profile:
                validate_positive_int(
                    profile[key],
                    f"resource profile {profile_name}.{key}",
                    allow_zero=key.endswith("pct"),
                )
        if "monitor" in profile and not isinstance(profile["monitor"], bool):
            raise PlannerError(
                f"resource profile {profile_name}.monitor must be boolean"
            )
        for key in ("test_threads", "low_disk_test_threads_max"):
            if key in profile:
                validate_positive_int(
                    profile[key], f"resource profile {profile_name}.{key}"
                )
        jobs_mode = profile.get("cargo_jobs_mode")
        if jobs_mode is not None and jobs_mode not in {"fixed", "auto"}:
            raise PlannerError(
                f"resource profile {profile_name}.cargo_jobs_mode must be fixed or auto"
            )
        jobs_default = profile.get("cargo_jobs_default")
        if jobs_default is not None and jobs_default not in {"min", "auto"}:
            raise PlannerError(
                f"resource profile {profile_name}.cargo_jobs_default must be min or auto"
            )
        for key in (
            "cargo_jobs_min",
            "cargo_jobs_max",
            "cargo_jobs_hard_max",
            "cargo_jobs_cpu_pct",
            "cargo_jobs_mem_per_job_mib",
            "cargo_jobs_low_disk_max",
        ):
            if key in profile:
                validate_positive_int(
                    profile[key], f"resource profile {profile_name}.{key}"
                )
        for key in ("cargo_jobs_cpu_reserve", "cargo_jobs_mem_reserve_mib"):
            if key in profile:
                validate_positive_int(
                    profile[key],
                    f"resource profile {profile_name}.{key}",
                    allow_zero=True,
                )
        jobs_min = profile.get("cargo_jobs_min")
        jobs_max = profile.get("cargo_jobs_max")
        jobs_hard_max = profile.get("cargo_jobs_hard_max")
        low_disk_jobs = profile.get("cargo_jobs_low_disk_max")
        if all(
            isinstance(value, int) and not isinstance(value, bool)
            for value in (jobs_min, jobs_max)
        ):
            if jobs_min > jobs_max:
                raise PlannerError(
                    f"resource profile {profile_name}.cargo_jobs_min must not exceed cargo_jobs_max"
                )
        if all(
            isinstance(value, int) and not isinstance(value, bool)
            for value in (jobs_max, jobs_hard_max)
        ):
            if jobs_max > jobs_hard_max:
                raise PlannerError(
                    f"resource profile {profile_name}.cargo_jobs_max must not exceed cargo_jobs_hard_max"
                )
        if all(
            isinstance(value, int) and not isinstance(value, bool)
            for value in (low_disk_jobs, jobs_max)
        ):
            if low_disk_jobs > jobs_max:
                raise PlannerError(
                    f"resource profile {profile_name}.cargo_jobs_low_disk_max must not exceed cargo_jobs_max"
                )

    for command_name, command in config.get("commands", {}).items():
        argv = command.get("argv")
        if not isinstance(argv, list) or not all(
            isinstance(item, str) for item in argv
        ):
            raise PlannerError(
                f"validation command {command_name!r} must define argv as a string array"
            )
        if raw_build_like_cargo(argv):
            raise PlannerError(
                f"validation command {command_name!r} uses raw build-like cargo; route through cargo-guard or just"
            )
        profile_name = command.get("profile")
        if profile_name is not None and profile_name not in profile_names:
            raise PlannerError(
                f"validation command {command_name!r} references unknown resource profile {profile_name!r}"
            )

    for surface in config.get("surfaces", []):
        surface_name = surface.get("name")
        if not isinstance(surface_name, str):
            raise PlannerError("each surface must define a string name")
        target_args = surface.get("target_args", [])
        if not isinstance(target_args, list) or not all(
            isinstance(item, str) for item in target_args
        ):
            raise PlannerError(
                f"surface {surface_name!r} target_args must be a string array"
            )

    for rule in config.get("path_rules", []):
        for package in rule.get("packages", []):
            if package not in package_names:
                raise PlannerError(f"path rule references unknown package {package!r}")
        for surface in rule.get("surfaces", []):
            if surface not in surface_names:
                raise PlannerError(f"path rule references unknown surface {surface!r}")
        for command_name in rule.get("commands", []):
            if command_name not in command_names:
                raise PlannerError(
                    f"path rule references unknown command {command_name!r}"
                )
        for command_name in rule.get("prep_commands", []):
            if command_name not in command_names:
                raise PlannerError(
                    f"path rule references unknown prep command {command_name!r}"
                )
        for generator in rule.get("generators", []):
            if generator not in command_names:
                raise PlannerError(
                    f"path rule references unknown generator command {generator!r}"
                )


def apply_path_rules(
    file_path: str, config: dict[str, Any], selection: Selection
) -> bool:
    file_surface_matched = False
    for rule in config.get("path_rules", []):
        patterns = rule.get("patterns", [])
        if not any(fnmatch(file_path, pattern) for pattern in patterns):
            continue
        reason = f"matched validation rule {', '.join(patterns)} for {file_path}"
        for package in rule.get("packages", []):
            selection.add_package(package, reason)
        for surface in rule.get("surfaces", []):
            selection.add_surface(surface, reason)
            file_surface_matched = True
        for flag in rule.get("flags", []):
            selection.flags.add(flag)
        for generator in rule.get("generators", []):
            selection.add_generator(generator, reason)
        for command_name in rule.get("prep_commands", []):
            selection.add_prep_command_name(command_name, reason)
        for command_name in rule.get("commands", []):
            selection.add_command_name(command_name, reason)
    return file_surface_matched


def classify_file(
    file_path: str,
    repo_root: Path,
    config: dict[str, Any],
    packages: list[PackageInfo],
    selection: Selection,
) -> None:
    file_surface_matched = apply_path_rules(file_path, config, selection)

    package = (
        package_for_file(file_path, repo_root, packages)
        if file_path.startswith("codex-rs/")
        else None
    )
    rust_source = file_path.endswith(".rs")
    cargo_manifest = (
        file_path.endswith("/Cargo.toml") or file_path == "codex-rs/Cargo.toml"
    )

    if rust_source:
        selection.flags.add("rust_source")
    if cargo_manifest:
        selection.flags.add("manifest_changed")

    if package and (rust_source or cargo_manifest):
        selection.add_package(package, f"{file_path} belongs to package {package}")

    if package and (rust_source or cargo_manifest) and not file_surface_matched:
        policy = config.get("defaults", {}).get("unknown_rust_path_policy")
        if policy == "package-plus-cli-strict":
            selection.add_surface(
                "cli", f"unknown Rust package path policy for {file_path}"
            )
            selection.add_unknown_path_fallback(file_path, package)

    deleted_rust_source = (
        rust_source
        and file_path.startswith("codex-rs/")
        and (
            is_git_deleted_path(repo_root, file_path)
            or (
                (path_evidence := selection.path_evidence.get(file_path)) is not None
                and path_evidence.saw_revision_deleted
                and not path_evidence.saw_revision_non_deleted
                and not path_evidence.saw_explicit_file
                and not os.path.lexists(repo_root / file_path)
            )
        )
    )
    if (
        rust_source
        and file_path.startswith("codex-rs/")
        and not package
        and not deleted_rust_source
    ):
        selection.feature_errors.append(
            f"Rust path {file_path} is not owned by a Cargo workspace package"
        )

    scan_text, hunk_scoped = feature_scan_text(repo_root, file_path)
    scope = "changed hunk" if hunk_scoped else "file"
    if (
        cargo_manifest
        and package
        and "[features]" in scan_text
        and not package_has_feature_profile(config, package)
    ):
        selection.feature_errors.append(
            f"{file_path} {scope} changes package {package} features; add a package_features profile before validation"
        )
    if (
        rust_source
        and package
        and contains_feature_cfg(scan_text)
        and not package_has_feature_profile(config, package)
    ):
        selection.feature_errors.append(
            f"{file_path} {scope} contains cfg(feature = ...); add a package_features profile for {package} before validation"
        )
    if (
        rust_source
        and package
        and hunk_scoped
        and changed_hunk_touches_feature_region(repo_root, file_path)
        and not package_has_feature_profile(config, package)
    ):
        selection.feature_errors.append(
            f"{file_path} changed hunk intersects cfg(feature = ...) region; add a package_features profile for {package} before validation"
        )


def command_from_config(
    config: dict[str, Any],
    command_name: str,
    reason: str,
    history_entries: list[dict[str, Any]],
) -> CommandEntry:
    command_config = config.get("commands", {}).get(command_name)
    if not command_config:
        raise PlannerError(
            f"validation command {command_name!r} is not defined in cargo-validation.toml"
        )
    argv = command_config.get("argv")
    if not isinstance(argv, list) or not all(isinstance(item, str) for item in argv):
        raise PlannerError(
            f"validation command {command_name!r} must define argv as a string array"
        )
    return command_with_profile(
        tuple(argv),
        reason,
        command_name,
        config,
        command_config.get("profile"),
        history_entries,
    )


def recipe_command(
    recipe: str,
    reason: str,
    config: dict[str, Any],
    profile: str | None,
    history_entries: list[dict[str, Any]],
) -> CommandEntry:
    return command_with_profile(
        ("just", recipe), reason, recipe, config, profile, history_entries
    )


def cargo_command(
    args: list[str],
    reason: str,
    config: dict[str, Any],
    profile: str,
    history_entries: list[dict[str, Any]],
    *,
    use_growth_history: bool = True,
) -> CommandEntry:
    return command_with_profile(
        ("./scripts/cargo-guard.sh", "cargo", *args),
        reason,
        "cargo",
        config,
        profile,
        history_entries,
        use_growth_history=use_growth_history,
    )


def force_no_clean_runtime(command: CommandEntry) -> CommandEntry:
    env = dict(command.env)
    env["CARGO_GUARD_EXPECTED_GROWTH_GIB"] = "0"
    env["CARGO_GUARD_NO_CLEAN"] = "1"
    return replace(
        command,
        env=env,
        effective_expected_growth_gib=0,
        expected_growth_source=FIRST_PARTY_RUNTIME_EXPECTED_GROWTH_SOURCE,
    )


def protect_produced_artifacts(command: CommandEntry) -> CommandEntry:
    env = dict(command.env)
    env["CARGO_GUARD_NO_POST_CLEAN"] = "1"
    return replace(command, env=env)


def add_command(
    commands: list[CommandEntry],
    command: CommandEntry,
    *,
    allow_duplicate: bool = False,
) -> None:
    if allow_duplicate:
        commands.append(command)
        return
    for existing in commands:
        if existing.argv == command.argv and existing.env == command.env:
            return
    commands.append(command)


def mode_at_least(mode: str, minimum: str) -> bool:
    return VALID_MODES.index(mode) >= VALID_MODES.index(minimum)


def surface_by_name(config: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {surface["name"]: surface for surface in config.get("surfaces", [])}


def runtime_test_args(package: str) -> list[str]:
    return ["test", "-p", package]


def add_first_party_runtime_support_binary_commands(
    commands: list[CommandEntry],
    config: dict[str, Any],
    history_entries: list[dict[str, Any]],
) -> None:
    # Merge-safety anchor: some runtime tests spawn first-party binaries via
    # codex_utils_cargo_bin (for example `codex --broker`). Keep the exact
    # helper build argv in cargo-validation.toml, and schedule it immediately
    # before every dependent runtime rung so clean-target validation does not
    # delete prepared helpers.
    add_command(
        commands,
        protect_produced_artifacts(
            command_from_config(
                config,
                FIRST_PARTY_RUNTIME_SUPPORT_BINS_COMMAND,
                "support binaries required by runtime tests",
                history_entries,
            )
        ),
        allow_duplicate=True,
    )


def build_plan(
    action: str,
    stage: str,
    mode: str,
    files: list[str],
    explicit_surfaces: list[str],
    repo_root: Path,
    config: dict[str, Any],
    packages: list[PackageInfo],
    receipt_dir: Path | None,
    telemetry_level: str,
    path_evidence: dict[str, PathSelectionEvidence] | None = None,
) -> Plan:
    history_entries = read_history_entries(
        receipt_dir / "history.jsonl" if receipt_dir else None
    )
    selection = Selection(files=files, path_evidence=path_evidence or {})
    for file_path in files:
        classify_file(file_path, repo_root, config, packages, selection)
    for surface in explicit_surfaces:
        selection.add_surface(surface, "explicit --surface selector")

    if selection.feature_errors:
        raise PlannerError("\n".join(selection.feature_errors))
    if mode_at_least(mode, "strict") and selection.unknown_path_fallbacks:
        raise PlannerError(
            "strict mode requires explicit validation path rules:\n"
            + "\n".join(selection.unknown_path_fallbacks)
        )

    commands: list[CommandEntry] = []
    manual: list[ManualEntry] = []
    selected_packages = sorted(selection.packages)
    selected_surfaces = sorted(selection.surfaces)
    package_infos = {package.name: package for package in packages}

    if stage == "prep" and (
        "rust_source" in selection.flags or "manifest_changed" in selection.flags
    ):
        add_command(
            commands,
            recipe_command(
                "fmt",
                "pre-review formatter materialization for Rust source or manifest change",
                config,
                None,
                history_entries,
            ),
        )

    need_test_targets = bool(
        {"runtime", "test_scope", "manifest_changed"} & selection.flags
    )
    need_runtime_tests = "runtime" in selection.flags

    if stage == "prep":
        for generator, reasons in sorted(selection.generators.items()):
            add_command(
                commands,
                command_from_config(
                    config,
                    generator,
                    "pre-review generated follower materialization: "
                    + "; ".join(sorted(reasons)),
                    history_entries,
                ),
            )
        for command_name in selection.prep_command_order:
            reasons = selection.prep_command_names[command_name]
            add_command(
                commands,
                command_from_config(
                    config,
                    command_name,
                    "pre-review mechanical materialization: "
                    + "; ".join(sorted(reasons)),
                    history_entries,
                ),
            )
        return Plan(
            action=action,
            stage=stage,
            mode=mode,
            files=files,
            selected_packages=selected_packages,
            selected_surfaces=selected_surfaces,
            flags=sorted(selection.flags),
            warnings=selection.warnings,
            commands=commands,
            manual=manual,
            receipt_dir=receipt_dir,
            telemetry_level=telemetry_level,
        )

    for command_name in selection.command_order:
        reasons = selection.command_names[command_name]
        add_command(
            commands,
            command_from_config(
                config, command_name, "; ".join(sorted(reasons)), history_entries
            ),
        )

    for package in selected_packages:
        package_info = package_infos[package]
        add_command(
            commands,
            cargo_command(
                ["check", "-p", package],
                f"selected package {package}",
                config,
                "check",
                history_entries,
            ),
        )
        if (
            mode_at_least(mode, "standard")
            and need_test_targets
            and package_info.has_test_targets
        ):
            add_command(
                commands,
                cargo_command(
                    ["check", "-p", package, "--tests"],
                    f"test target compilation needed for {package}",
                    config,
                    "check_tests",
                    history_entries,
                ),
            )
            add_command(
                commands,
                cargo_command(
                    ["test", "-p", package, "--no-run"],
                    f"test target build/link coverage needed for {package}",
                    config,
                    "test_no_run",
                    history_entries,
                ),
            )
        if (
            mode_at_least(mode, "standard")
            and need_runtime_tests
            and (package_info.has_test_targets or package_info.has_doctests)
        ):
            if package in FIRST_PARTY_RUNTIME_SUPPORT_PACKAGES:
                add_first_party_runtime_support_binary_commands(
                    commands, config, history_entries
                )
            add_command(
                commands,
                force_no_clean_runtime(
                    cargo_command(
                        runtime_test_args(package),
                        f"runtime behavior is in scope for {package}",
                        config,
                        "package_test",
                        history_entries,
                    )
                )
                if package in FIRST_PARTY_RUNTIME_SUPPORT_PACKAGES
                else cargo_command(
                    runtime_test_args(package),
                    f"runtime behavior is in scope for {package}",
                    config,
                    "package_test",
                    history_entries,
                ),
            )
        if mode_at_least(mode, "strict"):
            add_command(
                commands,
                command_with_profile(
                    ("just", "clippy-strict", "-p", package),
                    f"strict lint gate for {package}",
                    "clippy-strict",
                    config,
                    "clippy",
                    history_entries,
                ),
            )

    surfaces = surface_by_name(config)
    for surface_name in selected_surfaces:
        surface = surfaces.get(surface_name)
        if not surface:
            raise PlannerError(
                f"surface {surface_name!r} is not defined in cargo-validation.toml"
            )
        target_args = surface.get("target_args", [])
        if target_args:
            add_command(
                commands,
                cargo_command(
                    ["check", *target_args],
                    f"surface {surface_name} must match shipped target",
                    config,
                    "check",
                    history_entries,
                ),
            )
        if mode_at_least(mode, "strict"):
            for recipe in surface.get("strict_recipes", []):
                add_command(
                    commands,
                    recipe_command(
                        recipe,
                        f"strict surface gate for {surface_name}",
                        config,
                        "clippy",
                        history_entries,
                    ),
                )
            for recipe in surface.get("smoke_recipes", []):
                add_command(
                    commands,
                    recipe_command(
                        recipe,
                        f"smoke surface gate for {surface_name}",
                        config,
                        "build",
                        history_entries,
                    ),
                )

    if "snapshots" in selection.flags:
        manual.append(
            ManualEntry(
                "Review generated *.snap.new files before running cargo insta accept.",
                "snapshot-owning surface changed",
            )
        )

    if mode == "full":
        add_command(
            commands,
            recipe_command(
                "test",
                "full mode requests workspace nextest fan-in",
                config,
                "workspace_nextest",
                history_entries,
            ),
        )

    return Plan(
        action=action,
        stage=stage,
        mode=mode,
        files=files,
        selected_packages=selected_packages,
        selected_surfaces=selected_surfaces,
        flags=sorted(selection.flags),
        warnings=selection.warnings,
        commands=commands,
        manual=manual,
        receipt_dir=receipt_dir,
        telemetry_level=telemetry_level,
    )


def print_plan(plan: Plan, json_output: bool) -> None:
    if json_output:
        print(json.dumps(plan.to_json(), indent=2, sort_keys=True))
        return

    print("[cargo-validate][plan]")
    print(f"stage: {plan.stage}")
    print(f"mode: {plan.mode}")
    print(f"telemetry-level: {plan.telemetry_level}")
    if plan.files:
        print("changed:")
        for file_path in plan.files:
            print(f"  - {file_path}")
    if plan.selected_packages:
        print("packages: " + ", ".join(plan.selected_packages))
    if plan.selected_surfaces:
        print("surfaces: " + ", ".join(plan.selected_surfaces))
    if plan.flags:
        print("flags: " + ", ".join(plan.flags))
    if plan.warnings:
        print("warnings:")
        for warning in plan.warnings:
            print(f"  - {warning}")
    print("commands:")
    if not plan.commands:
        print("  (none)")
    for index, command in enumerate(plan.commands, start=1):
        print(f"  {index}. {' '.join(command.argv)}")
        print(f"     reason: {command.reason}")
        if command.env:
            env_text = " ".join(
                f"{key}={value}" for key, value in sorted(command.env.items())
            )
            print(f"     env: {env_text}")
    if plan.manual:
        print("manual:")
        for entry in plan.manual:
            print(f"  - {entry.message}")
            print(f"    reason: {entry.reason}")


def write_plan_receipt(plan: Plan, repo_root: Path) -> None:
    if not plan.receipt_dir:
        return
    plan.receipt_dir.mkdir(parents=True, exist_ok=True)
    tooling_digest = validation_tooling_digest(repo_root)
    payload = plan.to_json()
    payload["plan_id"] = plan_resume_id(plan, tooling_digest)
    payload["validation_tooling_digest"] = tooling_digest
    (plan.receipt_dir / "last-plan.json").write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n"
    )


def write_json_receipt(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")


def write_run_entry(receipt_dir: Path | None, entry: dict[str, Any]) -> None:
    if not receipt_dir:
        return
    receipt_dir.mkdir(parents=True, exist_ok=True)
    with (receipt_dir / "last-run.jsonl").open("a", encoding="utf-8") as run_file:
        run_file.write(json.dumps(entry, sort_keys=True) + "\n")


def receipt_relative_path(receipt_dir: Path, path: Path) -> str:
    return path.relative_to(receipt_dir).as_posix()


def command_log_paths(
    receipt_dir: Path,
    *,
    run_id: str,
    index: int,
    command_id: str,
) -> tuple[Path, Path]:
    log_dir = receipt_dir / COMMAND_LOG_DIR_NAME / run_id
    log_stem = f"{index:03d}-{command_id[:16]}"
    return log_dir / f"{log_stem}.stdout.log", log_dir / f"{log_stem}.stderr.log"


def command_telemetry_log_path(
    receipt_dir: Path,
    *,
    run_id: str,
    index: int,
    command_id: str,
) -> Path:
    log_dir = receipt_dir / COMMAND_LOG_DIR_NAME / run_id
    log_stem = f"{index:03d}-{command_id[:16]}"
    return log_dir / f"{log_stem}.telemetry.tsv"


@dataclass
class CommandOutputStream:
    name: str
    pipe: BinaryIO
    log_file: BinaryIO
    console: BinaryIO
    log_enabled: bool = True
    console_enabled: bool = True


def write_command_output_chunk(
    stream: CommandOutputStream,
    chunk: bytes,
    output_errors: list[str],
) -> None:
    if stream.log_enabled:
        try:
            write_all_command_output(stream.log_file, chunk)
            stream.log_file.flush()
        except Exception as error:
            output_errors.append(f"{stream.name} log: {error}")
            stream.log_enabled = False
    if stream.console_enabled:
        try:
            write_all_command_output(stream.console, chunk)
            stream.console.flush()
        except Exception as error:
            output_errors.append(f"{stream.name} console: {error}")
            stream.console_enabled = False


def write_all_command_output(sink: BinaryIO, chunk: bytes) -> None:
    bytes_written = 0
    while bytes_written < len(chunk):
        written = sink.write(chunk[bytes_written:])
        if written is None:
            raise OSError("output sink returned no byte count")
        if written <= 0:
            raise OSError(f"output sink wrote {written} bytes")
        bytes_written += written


def close_command_output_stream(stream: CommandOutputStream) -> None:
    try:
        stream.pipe.close()
    except (OSError, ValueError):
        pass


def pump_command_output(
    process: subprocess.Popen[bytes],
    *,
    stdout_log_file: BinaryIO,
    stderr_log_file: BinaryIO,
) -> int:
    if process.stdout is None or process.stderr is None:
        raise PlannerError("failed to open command output pipes")
    streams = {
        process.stdout.fileno(): CommandOutputStream(
            name="stdout",
            pipe=process.stdout,
            log_file=stdout_log_file,
            console=sys.stdout.buffer,
        ),
        process.stderr.fileno(): CommandOutputStream(
            name="stderr",
            pipe=process.stderr,
            log_file=stderr_log_file,
            console=sys.stderr.buffer,
        ),
    }
    for file_descriptor in streams:
        os.set_blocking(file_descriptor, False)

    output_errors: list[str] = []
    process_exit_at: float | None = None
    while streams:
        made_progress = False
        for file_descriptor, stream in list(streams.items()):
            try:
                chunk = os.read(file_descriptor, COMMAND_LOG_READ_CHUNK_BYTES)
            except BlockingIOError:
                continue
            except OSError as error:
                output_errors.append(f"{stream.name} pipe: {error}")
                close_command_output_stream(stream)
                del streams[file_descriptor]
                continue
            if not chunk:
                close_command_output_stream(stream)
                del streams[file_descriptor]
                continue
            made_progress = True
            write_command_output_chunk(stream, chunk, output_errors)

        return_code = process.poll()
        if return_code is not None:
            if process_exit_at is None:
                process_exit_at = time.monotonic()
            if (
                streams
                and time.monotonic() - process_exit_at
                >= COMMAND_LOG_PIPE_DRAIN_GRACE_SECONDS
            ):
                for stream in streams.values():
                    close_command_output_stream(stream)
                streams.clear()

        if not made_progress and streams:
            time.sleep(COMMAND_LOG_POLL_INTERVAL_SECONDS)

    return_code = process.wait()
    if output_errors:
        raise CommandOutputError(
            f"failed to record command output: {output_errors[0]}",
            return_code,
        )
    return return_code


def run_command_with_output_logs(
    argv: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    stdout_log_path: Path,
    stderr_log_path: Path,
) -> int:
    with contextlib.ExitStack() as stack:
        try:
            stdout_log_path.parent.mkdir(parents=True, exist_ok=True)
            stderr_log_path.parent.mkdir(parents=True, exist_ok=True)
            stdout_log_file = stack.enter_context(stdout_log_path.open("wb"))
            stderr_log_file = stack.enter_context(stderr_log_path.open("wb"))
        except OSError as error:
            raise PlannerError(
                f"failed to open command output logs: {error}"
            ) from error
        with subprocess.Popen(
            argv,
            cwd=cwd,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,
        ) as process:
            return pump_command_output(
                process,
                stdout_log_file=stdout_log_file,
                stderr_log_file=stderr_log_file,
            )


def read_run_entries(run_path: Path | None) -> list[dict[str, Any]]:
    if run_path is None or not run_path.is_file():
        return []
    entries: list[dict[str, Any]] = []
    for line in run_path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(entry, dict):
            entries.append(entry)
    return entries


def read_run_summary(receipt_dir: Path) -> dict[str, Any] | None:
    summary_path = receipt_dir / "last-run-summary.json"
    if not summary_path.is_file():
        return None
    try:
        summary = json.loads(summary_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise PlannerError(
            f"--resume cannot use malformed run summary {summary_path}: {error}"
        ) from error
    if not isinstance(summary, dict):
        raise PlannerError(
            f"--resume cannot use malformed run summary {summary_path}: expected JSON object"
        )
    return summary


def resume_summary_allows_reuse(
    receipt_dir: Path,
    run_path: Path,
    *,
    action: str,
    stage: str,
    plan_id: str,
    input_digest: str,
    tooling_digest: str,
    command_count: int,
) -> bool:
    summary = read_run_summary(receipt_dir)
    if summary is None:
        if run_path.exists():
            raise PlannerError(
                "--resume requires last-run-summary.json from a completed full run; "
                "run without --resume or pass --fresh to start a new full validation"
            )
        return False

    coverage = summary.get("coverage")
    if coverage != "full":
        raise PlannerError(
            "--resume requires the previous run summary to have coverage=full; "
            f"found coverage={coverage!r}. Run without --resume or pass --fresh to start a new full validation"
        )
    if summary.get("status") != 0:
        raise PlannerError(
            "--resume requires the previous run summary to have status=0; "
            f"found status={summary.get('status')!r}. Run without --resume or pass --fresh to start a new full validation"
        )
    if summary.get("partial_mode") is not None:
        raise PlannerError(
            "--resume requires a non-partial previous run summary; "
            f"found partial_mode={summary.get('partial_mode')!r}. Run without --resume or pass --fresh to start a new full validation"
        )

    return (
        summary.get("action") == action
        and summary.get("stage") == stage
        and summary.get("plan_id") == plan_id
        and summary.get("input_digest") == input_digest
        and summary.get("validation_tooling_digest") == tooling_digest
        and summary.get("command_count") == command_count
    )


def write_history_entry(receipt_dir: Path | None, entry: dict[str, Any]) -> None:
    if not receipt_dir:
        return
    receipt_dir.mkdir(parents=True, exist_ok=True)
    with (receipt_dir / "history.jsonl").open("a", encoding="utf-8") as history_file:
        history_file.write(json.dumps(entry, sort_keys=True) + "\n")


def direct_guard_command(command: CommandEntry) -> bool:
    return (
        len(command.argv) >= 3
        and command.argv[0] == "./scripts/cargo-guard.sh"
        and command.argv[1] == "cargo"
    )


def command_uses_guarded_cargo(command: CommandEntry) -> bool:
    if direct_guard_command(command):
        return True
    return (
        len(command.argv) >= 2
        and command.argv[0] == "just"
        and command.argv[1] in GUARDED_JUST_RECIPES
    )


def read_guard_metrics(metrics_path: Path) -> dict[str, Any]:
    try:
        metrics = json.loads(metrics_path.read_text(encoding="utf-8"))
    except OSError as error:
        raise PlannerError(
            f"guard metrics were not written: {metrics_path}: {error}"
        ) from error
    except json.JSONDecodeError as error:
        raise PlannerError(
            f"guard metrics are malformed JSON: {metrics_path}: {error}"
        ) from error
    if not isinstance(metrics, dict):
        raise PlannerError(f"guard metrics must be a JSON object: {metrics_path}")
    allowed_metric_keys = {
        "schema_version",
        "resource_profile",
        "command_fingerprint",
        "job_contract_digest",
        "cargo_subcommand",
        "jobs_mode",
        "jobs_default",
        "selected_cargo_build_job_cap",
        "effective_cargo_build_jobs",
        "effective_cargo_build_jobs_source",
        "selected_runtime_test_threads",
        "target_dir",
        "build_dir",
        "monitored_paths",
        "observed_growth_gib",
        "mem_available_selection_mib",
        "disk_emergency",
        "status",
        "telemetry_level",
        "telemetry_schema_version",
        "telemetry_log_path",
        "telemetry_sample_count",
        "telemetry_error_count",
        "top_rustc_crates",
    }
    unknown_metric_keys = sorted(set(metrics) - allowed_metric_keys)
    if unknown_metric_keys:
        raise PlannerError(
            f"guard metrics contain unknown top-level keys: {', '.join(unknown_metric_keys)}"
        )
    if metrics.get("schema_version") != 1:
        raise PlannerError(f"guard metrics schema_version must be 1: {metrics_path}")

    for key in (
        "resource_profile",
        "command_fingerprint",
        "job_contract_digest",
        "cargo_subcommand",
        "jobs_mode",
        "jobs_default",
        "effective_cargo_build_jobs_source",
        "target_dir",
        "build_dir",
        "telemetry_level",
    ):
        value = metrics.get(key)
        if not isinstance(value, str) or not value:
            raise PlannerError(
                f"guard metrics {key} must be a non-empty string: {metrics_path}"
            )
    for key in (
        "selected_cargo_build_job_cap",
        "effective_cargo_build_jobs",
        "observed_growth_gib",
        "status",
        "telemetry_schema_version",
        "telemetry_sample_count",
        "telemetry_error_count",
    ):
        value = metrics.get(key)
        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
            raise PlannerError(
                f"guard metrics {key} must be a non-negative integer: {metrics_path}"
            )
    if (
        metrics["selected_cargo_build_job_cap"] <= 0
        or metrics["effective_cargo_build_jobs"] <= 0
    ):
        raise PlannerError(
            f"guard metrics job counts must be positive integers: {metrics_path}"
        )
    if metrics["telemetry_schema_version"] <= 0:
        raise PlannerError(
            f"guard metrics telemetry_schema_version must be positive: {metrics_path}"
        )
    if metrics["jobs_default"] not in {"min", "auto"}:
        raise PlannerError(
            f"guard metrics jobs_default must be min or auto: {metrics_path}"
        )
    if metrics["telemetry_level"] not in TELEMETRY_LEVELS:
        raise PlannerError(f"guard metrics telemetry_level is invalid: {metrics_path}")
    telemetry_log_path = metrics.get("telemetry_log_path")
    if telemetry_log_path is not None and (
        not isinstance(telemetry_log_path, str) or not telemetry_log_path
    ):
        raise PlannerError(
            f"guard metrics telemetry_log_path must be null or a non-empty string: {metrics_path}"
        )
    test_threads = metrics.get("selected_runtime_test_threads")
    if test_threads is not None and (
        not isinstance(test_threads, int)
        or isinstance(test_threads, bool)
        or test_threads <= 0
    ):
        raise PlannerError(
            f"guard metrics selected_runtime_test_threads must be null or a positive integer: {metrics_path}"
        )
    mem_available = metrics.get("mem_available_selection_mib")
    if mem_available is not None and (
        not isinstance(mem_available, int)
        or isinstance(mem_available, bool)
        or mem_available < 0
    ):
        raise PlannerError(
            f"guard metrics mem_available_selection_mib must be null or a non-negative integer: {metrics_path}"
        )
    if not isinstance(metrics.get("disk_emergency"), bool):
        raise PlannerError(
            f"guard metrics disk_emergency must be boolean: {metrics_path}"
        )
    top_rustc_crates = metrics.get("top_rustc_crates")
    if not isinstance(top_rustc_crates, list):
        raise PlannerError(
            f"guard metrics top_rustc_crates must be a list: {metrics_path}"
        )
    allowed_crate_metric_keys = {
        "crate_name",
        "samples",
        "max_rss_kib",
        "sum_rss_kib",
    }
    if len(top_rustc_crates) > 10:
        raise PlannerError(
            f"guard metrics top_rustc_crates must be capped at 10 entries: {metrics_path}"
        )
    for index, crate_metrics in enumerate(top_rustc_crates, start=1):
        if not isinstance(crate_metrics, dict):
            raise PlannerError(
                f"guard metrics top_rustc_crates[{index}] must be an object: {metrics_path}"
            )
        unknown_crate_keys = sorted(set(crate_metrics) - allowed_crate_metric_keys)
        if unknown_crate_keys:
            raise PlannerError(
                f"guard metrics top_rustc_crates[{index}] contains unknown keys: {', '.join(unknown_crate_keys)}"
            )
        crate_name = crate_metrics.get("crate_name")
        if not isinstance(crate_name, str) or not crate_name:
            raise PlannerError(
                f"guard metrics top_rustc_crates[{index}].crate_name must be a non-empty string: {metrics_path}"
            )
        for key in ("samples", "max_rss_kib", "sum_rss_kib"):
            value = crate_metrics.get(key)
            if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                raise PlannerError(
                    f"guard metrics top_rustc_crates[{index}].{key} must be a non-negative integer: {metrics_path}"
                )
        if crate_metrics["samples"] <= 0:
            raise PlannerError(
                f"guard metrics top_rustc_crates[{index}].samples must be positive: {metrics_path}"
            )

    monitored_paths = metrics.get("monitored_paths")
    if not isinstance(monitored_paths, list) or not monitored_paths:
        raise PlannerError(
            f"guard metrics monitored_paths must be a non-empty list: {metrics_path}"
        )
    allowed_path_metric_keys = {
        "label",
        "path",
        "fs_id",
        "cleanable",
        "start_available_bytes",
        "min_available_bytes",
        "end_available_bytes",
        "observed_growth_bytes",
    }
    for index, path_metrics in enumerate(monitored_paths, start=1):
        if not isinstance(path_metrics, dict):
            raise PlannerError(
                f"guard metrics monitored_paths[{index}] must be an object: {metrics_path}"
            )
        unknown_path_keys = sorted(set(path_metrics) - allowed_path_metric_keys)
        if unknown_path_keys:
            raise PlannerError(
                f"guard metrics monitored_paths[{index}] contains unknown keys: {', '.join(unknown_path_keys)}"
            )
        for key in ("label", "path", "fs_id"):
            value = path_metrics.get(key)
            if not isinstance(value, str) or not value:
                raise PlannerError(
                    f"guard metrics monitored_paths[{index}].{key} must be a non-empty string: {metrics_path}"
                )
        if not isinstance(path_metrics.get("cleanable"), bool):
            raise PlannerError(
                f"guard metrics monitored_paths[{index}].cleanable must be boolean: {metrics_path}"
            )
        for key in (
            "start_available_bytes",
            "min_available_bytes",
            "end_available_bytes",
            "observed_growth_bytes",
        ):
            value = path_metrics.get(key)
            if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                raise PlannerError(
                    f"guard metrics monitored_paths[{index}].{key} must be a non-negative integer: {metrics_path}"
                )
    return metrics


def latest_matching_run_entries(
    entries: list[dict[str, Any]],
    *,
    action: str,
    stage: str,
    plan_id: str,
    input_digest: str,
    tooling_digest: str,
) -> dict[str, dict[str, Any]]:
    latest: dict[str, dict[str, Any]] = {}
    for entry in entries:
        if entry.get("action") != action:
            continue
        if entry.get("stage") != stage:
            continue
        if entry.get("plan_id") != plan_id:
            continue
        if entry.get("input_digest") != input_digest:
            continue
        if entry.get("validation_tooling_digest") != tooling_digest:
            continue
        command_key = entry.get("command_key")
        if isinstance(command_key, str):
            latest[command_key] = entry
    return latest


def successful_resume_source(entry: dict[str, Any] | None) -> bool:
    if not successful_run_entry(entry):
        return False
    if entry.get("partial_mode") is not None:
        return False
    if entry.get("coverage") == "executed":
        return True
    return (
        entry.get("coverage") == "skipped" and entry.get("coverage_source") == "resume"
    )


def successful_run_entry(entry: dict[str, Any] | None) -> bool:
    if entry is None or entry.get("status") != 0:
        return False
    if entry.get("guard_metrics_error") is not None:
        return False
    if entry.get("output_log_error") is not None:
        return False
    return True


def write_run_summary(receipt_dir: Path | None, summary: dict[str, Any]) -> None:
    if not receipt_dir:
        return
    write_json_receipt(receipt_dir / "last-run-summary.json", summary)


def verify_plan(
    plan: Plan,
    repo_root: Path,
    keep_going: bool,
    *,
    resume: bool = False,
    fresh: bool = False,
    from_index: int | None = None,
    only_failed: bool = False,
    explain_skip: bool = False,
) -> int:
    if from_index is not None:
        if from_index < 1:
            raise PlannerError("--from-index must be >= 1")
        if from_index > len(plan.commands):
            raise PlannerError(
                f"--from-index {from_index} exceeds plan length {len(plan.commands)}"
            )
    if fresh and (resume or only_failed or from_index is not None):
        raise PlannerError(
            "--fresh cannot be combined with --resume, --only-failed, or --from-index"
        )
    if resume and only_failed:
        raise PlannerError("--resume and --only-failed are mutually exclusive")
    if resume and from_index is not None:
        raise PlannerError("--resume and --from-index are mutually exclusive")
    if only_failed and from_index is not None:
        raise PlannerError("--only-failed and --from-index are mutually exclusive")

    tooling_digest = validation_tooling_digest(repo_root)
    current_plan_id = plan_resume_id(plan, tooling_digest)
    current_input_digest = plan_input_digest(plan, repo_root)
    run_id = stable_digest(
        {
            "schema": 1,
            "plan_id": current_plan_id,
            "input_digest": current_input_digest,
            "validation_tooling_digest": tooling_digest,
            "started_at": time.time(),
            "pid": os.getpid(),
        }
    )

    previous_entries: list[dict[str, Any]] = []
    run_path: Path | None = None
    if plan.receipt_dir:
        run_path = plan.receipt_dir / "last-run.jsonl"
        if resume:
            if resume_summary_allows_reuse(
                plan.receipt_dir,
                run_path,
                action=plan.action,
                stage=plan.stage,
                plan_id=current_plan_id,
                input_digest=current_input_digest,
                tooling_digest=tooling_digest,
                command_count=len(plan.commands),
            ):
                previous_entries = read_run_entries(run_path)
        elif only_failed:
            previous_entries = read_run_entries(run_path)
        run_path.parent.mkdir(parents=True, exist_ok=True)
        run_path.write_text("")
        command_log_dir = plan.receipt_dir / COMMAND_LOG_DIR_NAME / run_id
        write_run_summary(
            plan.receipt_dir,
            {
                "schema_version": 1,
                "action": plan.action,
                "stage": plan.stage,
                "coverage": "in_progress",
                "mode": plan.mode,
                "telemetry_level": plan.telemetry_level,
                "run_id": run_id,
                "plan_id": current_plan_id,
                "input_digest": current_input_digest,
                "validation_tooling_digest": tooling_digest,
                "command_count": len(plan.commands),
                "command_log_dir": receipt_relative_path(
                    plan.receipt_dir, command_log_dir
                ),
                "started_at": time.time(),
                "partial_mode": "from-index"
                if from_index is not None
                else "only-failed"
                if only_failed
                else None,
            },
        )

    latest_entries = latest_matching_run_entries(
        previous_entries,
        action=plan.action,
        stage=plan.stage,
        plan_id=current_plan_id,
        input_digest=current_input_digest,
        tooling_digest=tooling_digest,
    )

    exit_status = 0
    records: list[dict[str, Any]] = []
    stopped_after_failure = False
    current_partial_mode = (
        "from-index"
        if from_index is not None
        else "only-failed"
        if only_failed
        else None
    )
    for index, command in enumerate(plan.commands, start=1):
        current_command_id = command_resume_id(command)
        current_command_key = command_resume_key(index, command)
        latest_entry = latest_entries.get(current_command_key)

        skip_reason: str | None = None
        skip_status: int | None = None
        coverage_source: str | None = None
        skip_source_run_id: str | None = None
        skip_source_started_at: float | None = None
        if from_index is not None and index < from_index:
            skip_reason = f"before --from-index {from_index}"
            coverage_source = "partial:before-index"
        elif only_failed:
            if latest_entry is None:
                skip_reason = "no previous matching failure"
                coverage_source = "partial:no-previous-failure"
            elif successful_run_entry(latest_entry):
                skip_reason = "previous matching run passed"
                coverage_source = "partial:previous-pass"
            elif latest_entry.get("status") is None:
                skip_reason = "previous matching run did not execute"
                coverage_source = "partial:previous-uncovered"
        elif resume and successful_resume_source(latest_entry):
            skip_reason = "previous matching run passed"
            coverage_source = "resume"
            skip_status = 0
            skip_source_run_id = (
                latest_entry.get("run_id")
                if isinstance(latest_entry.get("run_id"), str)
                else None
            )
            candidate_started_at = latest_entry.get("started_at")
            skip_source_started_at = (
                candidate_started_at
                if isinstance(candidate_started_at, (int, float))
                and not isinstance(candidate_started_at, bool)
                else None
            )

        if skip_reason is not None:
            message = f"[cargo-validate][skip] {index}/{len(plan.commands)} {' '.join(command.argv)}"
            if explain_skip:
                message += f" ({skip_reason})"
            print(message)
            entry = {
                "coverage": "skipped",
                "coverage_source": coverage_source,
                "partial_mode": current_partial_mode,
                "skip_reason": skip_reason,
                "skip_source_run_id": skip_source_run_id,
                "skip_source_started_at": skip_source_started_at,
                "index": index,
                "command_id": current_command_id,
                "command_key": current_command_key,
                "action": plan.action,
                "stage": plan.stage,
                "plan_id": current_plan_id,
                "input_digest": current_input_digest,
                "validation_tooling_digest": tooling_digest,
                "run_id": run_id,
                "argv": list(command.argv),
                "reason": command.reason,
                "env": dict(sorted(command.env.items())),
                "resource_profile": command.resource_profile,
                "fingerprint": command.fingerprint,
                "job_contract_digest": command.job_contract_digest,
                "fallback_expected_growth_gib": command.fallback_expected_growth_gib,
                "effective_expected_growth_gib": command.effective_expected_growth_gib,
                "expected_growth_source": command.expected_growth_source,
                "status": skip_status,
            }
            write_run_entry(plan.receipt_dir, entry)
            records.append(entry)
            continue

        print(
            f"[cargo-validate][run] {index}/{len(plan.commands)} {' '.join(command.argv)}"
        )
        started_at = time.time()
        command_env = os.environ.copy()
        if "CARGO_GUARD_EXPECTED_GROWTH_GIB" not in command.env:
            command_env.pop("CARGO_GUARD_EXPECTED_GROWTH_GIB", None)
        command_env.update(command.env)
        metrics_path: Path | None = None
        telemetry_log_path: Path | None = None
        uses_guarded_cargo = command_uses_guarded_cargo(command)
        if direct_guard_command(command) and plan.receipt_dir:
            if command.fingerprint is None:
                raise PlannerError(
                    f"direct guarded command {index} is missing a command fingerprint"
                )
            metrics_path = plan.receipt_dir / f".guard-metrics-{index}.json"
            command_env["CARGO_GUARD_METRICS_PATH"] = str(metrics_path)
            command_env["CARGO_GUARD_COMMAND_FINGERPRINT"] = command.fingerprint
        if uses_guarded_cargo and plan.receipt_dir:
            command_env["CARGO_GUARD_TELEMETRY_LEVEL"] = plan.telemetry_level
            if plan.telemetry_level != "off":
                telemetry_log_path = command_telemetry_log_path(
                    plan.receipt_dir,
                    run_id=run_id,
                    index=index,
                    command_id=current_command_id,
                )
                command_env["CARGO_GUARD_TELEMETRY_PATH"] = str(telemetry_log_path)
        stdout_log_path: Path | None = None
        stderr_log_path: Path | None = None
        output_log_error: str | None = None
        if plan.receipt_dir:
            stdout_log_path, stderr_log_path = command_log_paths(
                plan.receipt_dir,
                run_id=run_id,
                index=index,
                command_id=current_command_id,
            )
            try:
                command_status = run_command_with_output_logs(
                    command.argv,
                    cwd=repo_root,
                    env=command_env,
                    stdout_log_path=stdout_log_path,
                    stderr_log_path=stderr_log_path,
                )
            except CommandOutputError as error:
                command_status = error.return_code
                output_log_error = str(error)
        else:
            command_status = subprocess.run(
                command.argv, cwd=repo_root, env=command_env, check=False
            ).returncode
        finished_at = time.time()
        entry = {
            "index": index,
            "argv": list(command.argv),
            "reason": command.reason,
            "env": dict(sorted(command.env.items())),
            "resource_profile": command.resource_profile,
            "fingerprint": command.fingerprint,
            "job_contract_digest": command.job_contract_digest,
            "coverage": "executed",
            "coverage_source": "executed",
            "partial_mode": current_partial_mode,
            "command_id": current_command_id,
            "command_key": current_command_key,
            "action": plan.action,
            "stage": plan.stage,
            "plan_id": current_plan_id,
            "input_digest": current_input_digest,
            "validation_tooling_digest": tooling_digest,
            "run_id": run_id,
            "fallback_expected_growth_gib": command.fallback_expected_growth_gib,
            "effective_expected_growth_gib": command.effective_expected_growth_gib,
            "expected_growth_source": command.expected_growth_source,
            "status": command_status,
            "started_at": started_at,
            "duration_seconds": round(finished_at - started_at, 3),
        }
        if (
            plan.receipt_dir
            and stdout_log_path is not None
            and stderr_log_path is not None
        ):
            entry["stdout_log_path"] = receipt_relative_path(
                plan.receipt_dir, stdout_log_path
            )
            entry["stderr_log_path"] = receipt_relative_path(
                plan.receipt_dir, stderr_log_path
            )
        if plan.receipt_dir and telemetry_log_path is not None:
            entry["telemetry_log_path"] = receipt_relative_path(
                plan.receipt_dir, telemetry_log_path
            )
        if output_log_error is not None:
            entry["output_log_error"] = output_log_error
            print(f"[cargo-validate][error] {output_log_error}", file=sys.stderr)
        metrics: dict[str, Any] | None = None
        metrics_error: str | None = None
        if metrics_path is not None:
            try:
                metrics = read_guard_metrics(metrics_path)
                if metrics.get("command_fingerprint") != command.fingerprint:
                    raise PlannerError(
                        f"guard metrics fingerprint mismatch for command {index}"
                    )
                if metrics.get("resource_profile") != command.resource_profile:
                    raise PlannerError(
                        f"guard metrics resource_profile mismatch for command {index}"
                    )
                if metrics.get("job_contract_digest") != command.job_contract_digest:
                    raise PlannerError(
                        f"guard metrics job_contract_digest mismatch for command {index}"
                    )
            except PlannerError as error:
                metrics_error = str(error)
                entry["guard_metrics_error"] = metrics_error
                print(f"[cargo-validate][error] {metrics_error}", file=sys.stderr)
            finally:
                metrics_path.unlink(missing_ok=True)
            if metrics_error is None:
                entry["guard_metrics"] = metrics
        write_run_entry(plan.receipt_dir, entry)
        records.append(entry)
        if output_log_error is not None:
            exit_status = 2
            print(
                f"[cargo-validate][error] stopping after failed command {index}",
                file=sys.stderr,
            )
            stopped_after_failure = True
            break
        if metrics_error is not None:
            exit_status = 2
            if not keep_going:
                print(
                    f"[cargo-validate][error] stopping after failed command {index}",
                    file=sys.stderr,
                )
                stopped_after_failure = True
                break
            continue
        if metrics is not None and (
            command_status == 0 or metrics.get("disk_emergency") is True
        ):
            risk_kind = (
                "disk_emergency" if metrics.get("disk_emergency") is True else "success"
            )
            history_entry = {
                "argv": list(command.argv),
                "fingerprint": command.fingerprint,
                "job_contract_digest": metrics.get("job_contract_digest"),
                "resource_profile": command.resource_profile,
                "risk_kind": risk_kind,
                "status": command_status,
                "disk_emergency": metrics.get("disk_emergency"),
                "observed_growth_gib": metrics.get("observed_growth_gib"),
                "selected_jobs": metrics.get("effective_cargo_build_jobs"),
                "selected_jobs_source": metrics.get(
                    "effective_cargo_build_jobs_source"
                ),
                "jobs_default": metrics.get("jobs_default"),
                "test_threads": metrics.get("selected_runtime_test_threads"),
                "duration_seconds": round(finished_at - started_at, 3),
                "recorded_at": finished_at,
            }
            write_history_entry(plan.receipt_dir, history_entry)
        if command_status != 0:
            exit_status = command_status
            if not keep_going:
                print(
                    f"[cargo-validate][error] stopping after failed command {index}",
                    file=sys.stderr,
                )
                stopped_after_failure = True
                break

    if stopped_after_failure:
        for index in range(len(records) + 1, len(plan.commands) + 1):
            command = plan.commands[index - 1]
            entry = {
                "coverage": "skipped",
                "coverage_source": "not-run-after-failure",
                "partial_mode": current_partial_mode,
                "skip_reason": "not run after earlier failure",
                "index": index,
                "command_id": command_resume_id(command),
                "command_key": command_resume_key(index, command),
                "action": plan.action,
                "stage": plan.stage,
                "plan_id": current_plan_id,
                "input_digest": current_input_digest,
                "validation_tooling_digest": tooling_digest,
                "run_id": run_id,
                "argv": list(command.argv),
                "reason": command.reason,
                "env": dict(sorted(command.env.items())),
                "resource_profile": command.resource_profile,
                "fingerprint": command.fingerprint,
                "job_contract_digest": command.job_contract_digest,
                "fallback_expected_growth_gib": command.fallback_expected_growth_gib,
                "effective_expected_growth_gib": command.effective_expected_growth_gib,
                "expected_growth_source": command.expected_growth_source,
                "status": None,
            }
            write_run_entry(plan.receipt_dir, entry)
            records.append(entry)

    full_coverage = (
        current_partial_mode is None
        and len(records) == len(plan.commands)
        and all(successful_run_entry(entry) for entry in records)
    )
    summary = {
        "schema_version": 1,
        "action": plan.action,
        "stage": plan.stage,
        "coverage": "full" if full_coverage else "partial",
        "mode": plan.mode,
        "telemetry_level": plan.telemetry_level,
        "run_id": run_id,
        "plan_id": current_plan_id,
        "input_digest": current_input_digest,
        "validation_tooling_digest": tooling_digest,
        "command_count": len(plan.commands),
        "command_log_dir": receipt_relative_path(
            plan.receipt_dir, plan.receipt_dir / COMMAND_LOG_DIR_NAME / run_id
        )
        if plan.receipt_dir
        else None,
        "covered_count": sum(1 for entry in records if successful_run_entry(entry)),
        "executed_count": sum(
            1 for entry in records if entry.get("coverage") == "executed"
        ),
        "skipped_count": sum(
            1 for entry in records if entry.get("coverage") == "skipped"
        ),
        "partial_mode": current_partial_mode,
        "status": exit_status,
        "failed_commands": [
            {
                "index": entry.get("index"),
                "status": entry.get("status"),
                "argv": entry.get("argv"),
            }
            for entry in records
            if entry.get("status") not in (0, None)
            or entry.get("guard_metrics_error") is not None
            or entry.get("output_log_error") is not None
        ],
        "output_log_failures": [
            {
                "index": entry.get("index"),
                "status": entry.get("status"),
                "argv": entry.get("argv"),
                "error": entry.get("output_log_error"),
            }
            for entry in records
            if entry.get("output_log_error") is not None
        ],
        "uncovered_commands": [
            {
                "index": entry.get("index"),
                "coverage_source": entry.get("coverage_source"),
                "argv": entry.get("argv"),
            }
            for entry in records
            if entry.get("status") is None
        ],
        "finished_at": time.time(),
    }
    write_run_summary(plan.receipt_dir, summary)
    return exit_status


def build_arg_parser(default_mode: str) -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="cargo-validate")
    parser.add_argument("action", choices=("plan", "verify", "prep-plan", "prep"))
    parser.add_argument(
        "--changed",
        action="store_true",
        help="plan from tracked and untracked Git changes",
    )
    parser.add_argument(
        "--file",
        dest="files",
        action="append",
        default=[],
        help="add an explicit changed file",
    )
    parser.add_argument(
        "--commit",
        dest="commits",
        action="append",
        default=[],
        help="add files changed by one non-merge commit",
    )
    parser.add_argument(
        "--range",
        dest="ranges",
        action="append",
        default=[],
        help="add files changed by a Git revision range, for example BASE..HEAD",
    )
    parser.add_argument(
        "--surface",
        dest="surfaces",
        action="append",
        default=[],
        help="add an explicit validation surface",
    )
    parser.add_argument("--mode", choices=VALID_MODES, default=default_mode)
    parser.add_argument(
        "--telemetry-level",
        choices=TELEMETRY_LEVELS,
        default=DEFAULT_TELEMETRY_LEVEL,
        help="guard telemetry detail for validation runs",
    )
    parser.add_argument(
        "--fail-fast",
        dest="keep_going",
        action="store_false",
        default=True,
        help="stop verify after the first command failure instead of collecting all reachable failures",
    )
    parser.add_argument(
        "--resume",
        action="store_true",
        help="skip commands covered by matching successful receipts",
    )
    parser.add_argument(
        "--fresh",
        action="store_true",
        help="ignore prior run receipts and start a fresh receipt",
    )
    parser.add_argument(
        "--from-index",
        type=int,
        help="run only commands from this 1-based index and write a partial receipt",
    )
    parser.add_argument(
        "--only-failed",
        action="store_true",
        help="run only commands that failed in a matching prior receipt",
    )
    parser.add_argument(
        "--explain-skip",
        action="store_true",
        help="include skip reasons in verify output",
    )
    parser.add_argument(
        "--json", action="store_true", help="print machine-readable JSON plan"
    )
    parser.add_argument("--config", type=Path, help="validation map override")
    parser.add_argument(
        "--metadata-json", type=Path, help="cargo metadata JSON fixture"
    )
    parser.add_argument(
        "--no-receipt",
        action="store_true",
        help="do not write last-plan/last-run receipts",
    )
    parser.add_argument("--receipt-dir", type=Path, help="override receipt directory")
    parser.add_argument("--repo-root", type=Path, help=argparse.SUPPRESS)
    return parser


def parse_args(argv: list[str]) -> argparse.Namespace:
    pre_parser = argparse.ArgumentParser(add_help=False)
    pre_parser.add_argument("--config", type=Path)
    known, _remaining = pre_parser.parse_known_args(argv)
    config_path = known.config or (
        repo_root_from_script() / "scripts" / "cargo-validation.toml"
    )
    config = load_config(config_path)
    default_mode = config.get("defaults", {}).get("standard_mode", "standard")
    parser = build_arg_parser(default_mode)
    args = parser.parse_args(argv)
    args.loaded_config = config
    args.loaded_config_path = config_path
    return args


def action_stage(action: str) -> str:
    if action in PREP_ACTIONS:
        return "prep"
    if action in VALIDATION_ACTIONS:
        return "validation"
    raise PlannerError(f"unsupported action {action!r}")


def main(argv: list[str]) -> int:
    try:
        args = parse_args(argv)
        repo_root = (args.repo_root or repo_root_from_script()).resolve()
        config = args.loaded_config
        stage = action_stage(args.action)

        files: list[str] = []
        seen: set[str] = set()
        path_evidence: dict[str, PathSelectionEvidence] = {}
        if args.changed:
            add_selected_files(files, seen, git_changed_files(repo_root), repo_root)
        for revision in args.commits:
            add_revision_files(
                files,
                seen,
                git_commit_files(repo_root, revision),
                path_evidence,
            )
        for revision_range in args.ranges:
            add_revision_files(
                files,
                seen,
                git_range_files(repo_root, revision_range),
                path_evidence,
            )
        add_selected_files(
            files,
            seen,
            args.files,
            repo_root,
            path_evidence=path_evidence,
            mark_explicit=True,
        )

        if not files and not args.surfaces:
            raise PlannerError("no changed files or --surface selectors supplied")

        packages = load_metadata(repo_root, args.metadata_json)
        validate_config(config, packages)

        receipt_dir = None
        if not args.no_receipt:
            receipt_dir = (
                args.receipt_dir
                or (
                    repo_root
                    / config.get("receipts", {}).get("dir", ".sangoi/validation")
                )
            ).resolve()

        plan = build_plan(
            action=args.action,
            stage=stage,
            mode=args.mode,
            files=files,
            explicit_surfaces=args.surfaces,
            repo_root=repo_root,
            config=config,
            packages=packages,
            receipt_dir=receipt_dir,
            telemetry_level=args.telemetry_level,
            path_evidence=path_evidence,
        )
        write_plan_receipt(plan, repo_root)
        print_plan(plan, args.json)
        if args.action in PLAN_ACTIONS:
            return 0
        return verify_plan(
            plan,
            repo_root,
            args.keep_going,
            resume=args.resume,
            fresh=args.fresh,
            from_index=args.from_index,
            only_failed=args.only_failed,
            explain_skip=args.explain_skip,
        )
    except PlannerError as error:
        print(f"[cargo-validate][error] {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
