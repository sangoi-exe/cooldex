#!/usr/bin/env python3
"""Regression coverage for the native Windows manifest executor."""

import hashlib
import json
import os
import selectors
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import tomllib
import unittest
import uuid
import zipfile
from pathlib import Path

# Merge-safety anchor: this harness proves direct manifest argv, strict runtime
# parsing, and disposable indexed-candidate materialization without writing F:
# from Python or starting a Rust build.

REPO_ROOT = Path(__file__).resolve().parents[1]
HELPER = REPO_ROOT / "scripts" / "cargo-validate-windows.ps1"
PRODUCTION_CONFIG = REPO_ROOT / "scripts" / "cargo-validation.toml"
WINDOWS_WORKSPACE_COMMAND_NAME = "windows-nextest-workspace"
PWSH = "/mnt/c/Program Files/PowerShell/7/pwsh.exe"


def production_windows_workspace_argv() -> list[str]:
    command = tomllib.loads(PRODUCTION_CONFIG.read_text(encoding="utf-8"))["commands"][
        WINDOWS_WORKSPACE_COMMAND_NAME
    ]
    argv = command["argv"]
    if not isinstance(argv, list) or not all(
        isinstance(argument, str) for argument in argv
    ):
        raise AssertionError("production Windows workspace aggregate argv is malformed")
    return list(argv)


TEST_SCRATCH_PARENT = Path.home() / ".cache" / "codex" / "cargo-validate-windows-tests"
TEST_ONLY_FAKE_CARGO_OPT_IN = "CARGO_VALIDATE_WINDOWS_TEST_ONLY_FAKE_CARGO"
TEST_ONLY_PREFLIGHT_FIXTURE_OPT_IN = (
    "CARGO_VALIDATE_WINDOWS_TEST_ONLY_PREFLIGHT_FIXTURES"
)
TEST_ONLY_FAKE_CREATE_IGNORED = "CARGO_VALIDATE_WINDOWS_TEST_FAKE_CREATE_IGNORED"
TEST_ONLY_FAKE_CREATE_UNTRACKED = "CARGO_VALIDATE_WINDOWS_TEST_FAKE_CREATE_UNTRACKED"
TEST_ONLY_FAKE_CREATE_INSTA_PENDING = (
    "CARGO_VALIDATE_WINDOWS_TEST_FAKE_CREATE_INSTA_PENDING"
)
TEST_ONLY_FAKE_LONG_RUNNING = "CARGO_VALIDATE_WINDOWS_TEST_FAKE_LONG_RUNNING"
TEST_ONLY_FAKE_ROOT_EXITS_FIRST = "CARGO_VALIDATE_WINDOWS_TEST_FAKE_ROOT_EXITS_FIRST"
TEST_ONLY_FAKE_IMMEDIATE_DESCENDANT = (
    "CARGO_VALIDATE_WINDOWS_TEST_FAKE_IMMEDIATE_DESCENDANT"
)
TEST_ONLY_FAKE_CONTAINMENT_TERMINATE_FAILURE = (
    "CARGO_VALIDATE_WINDOWS_TEST_FAKE_CONTAINMENT_TERMINATE_FAILURE"
)
TEST_ONLY_FAKE_WAIT_FOR_READINESS = (
    "CARGO_VALIDATE_WINDOWS_TEST_FAKE_WAIT_FOR_READINESS"
)
TEST_ONLY_FAKE_SUPPRESS_READINESS = (
    "CARGO_VALIDATE_WINDOWS_TEST_FAKE_SUPPRESS_READINESS"
)
TEST_ONLY_PREFLIGHT_FAIL_STAGE = "CARGO_VALIDATE_WINDOWS_TEST_PREFLIGHT_FAIL_STAGE"
TEST_ONLY_PATH_OWNERSHIP = "CARGO_VALIDATE_WINDOWS_TEST_PATH_OWNERSHIP"
TEST_ONLY_BOOTSTRAP_FIXTURE_OPT_IN = (
    "CARGO_VALIDATE_WINDOWS_TEST_ONLY_BOOTSTRAP_FIXTURES"
)
NATIVE_MUTEX_NAME = r"Local\Cooldex.WindowsBuildCacheCleanup.v1"
FIXTURE_NAMESPACE_PREFIX = "cw-test-"
PREFLIGHT_FIXTURE_FIELDS = (
    "CARGO_VALIDATE_WINDOWS_TEST_DISK_FREE_BYTES",
    "CARGO_VALIDATE_WINDOWS_TEST_DISK_TOTAL_BYTES",
    "CARGO_VALIDATE_WINDOWS_TEST_AVAILABLE_MEMORY_BYTES",
    "CARGO_VALIDATE_WINDOWS_TEST_NATIVE_PROCESS_STATE",
    "CARGO_VALIDATE_WINDOWS_TEST_WSL_PROCESS_STATE",
    "CARGO_VALIDATE_WINDOWS_TEST_MUTEX_STATE",
    "CARGO_VALIDATE_WINDOWS_TEST_CACHE_ROOT_STATE",
    "CARGO_VALIDATE_WINDOWS_TEST_MONITOR_ERROR_AFTER_SAMPLES",
)
DEFAULT_PREFLIGHT_FIXTURE = {
    "CARGO_VALIDATE_WINDOWS_TEST_DISK_FREE_BYTES": str(128 * 1024**3),
    "CARGO_VALIDATE_WINDOWS_TEST_DISK_TOTAL_BYTES": str(256 * 1024**3),
    "CARGO_VALIDATE_WINDOWS_TEST_AVAILABLE_MEMORY_BYTES": str(64 * 1024**3),
    "CARGO_VALIDATE_WINDOWS_TEST_NATIVE_PROCESS_STATE": "clear",
    "CARGO_VALIDATE_WINDOWS_TEST_WSL_PROCESS_STATE": "clear",
    "CARGO_VALIDATE_WINDOWS_TEST_MUTEX_STATE": "clear",
    "CARGO_VALIDATE_WINDOWS_TEST_CACHE_ROOT_STATE": "cleared",
    "CARGO_VALIDATE_WINDOWS_TEST_MONITOR_ERROR_AFTER_SAMPLES": "0",
}
BOOTSTRAP_FIXTURE_FIELDS = (
    "CARGO_VALIDATE_WINDOWS_TEST_BOOTSTRAP_NEXTEST_ZIP",
    "CARGO_VALIDATE_WINDOWS_TEST_BOOTSTRAP_V8_ARCHIVE",
    "CARGO_VALIDATE_WINDOWS_TEST_BOOTSTRAP_V8_BINDING",
    "CARGO_VALIDATE_WINDOWS_TEST_BOOTSTRAP_FINAL_HOST",
)

NATIVE_FIXTURE_PATH_LIFETIME_COMMAND = r"""
$ErrorActionPreference = "Stop"
$root = [System.IO.Path]::GetFullPath("F:\.cache").TrimEnd("\")
$token = $env:CARGO_VALIDATE_WINDOWS_TEST_FIXTURE_CLEANUP_TOKEN
$action = $env:CARGO_VALIDATE_WINDOWS_TEST_FIXTURE_CLEANUP_ACTION

if ($token -notmatch "^[a-f0-9]{32}$") {
    throw "fixture cleanup token must be 32 lowercase hexadecimal characters"
}
if ($action -notin @("assert-absent", "assert-present", "cleanup")) {
    throw "fixture cleanup action is unsupported"
}

function Get-FixtureNames {
    param([string]$Raw, [string]$Kind)

    try {
        $values = @(ConvertFrom-Json -InputObject $Raw)
    } catch {
        throw "fixture cleanup $Kind list is not valid JSON"
    }
    foreach ($value in $values) {
        if ($value -isnot [string]) {
            throw "fixture cleanup $Kind entries must be strings"
        }
    }
    return $values
}

function Get-OwnedFixturePath {
    param([string]$Name, [string]$Kind)

    $pattern = if ($Kind -eq "namespace") {
        "^cw-test-$token-[a-f0-9]{8}$"
    } else {
        "^p$token[a-f0-9]{8}$"
    }
    if ($Name -notmatch $pattern) {
        throw "fixture cleanup $Kind is not bound to this test token"
    }
    $path = [System.IO.Path]::GetFullPath((Join-Path $root $Name))
    if (-not [string]::Equals([System.IO.Path]::GetDirectoryName($path), $root, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "fixture cleanup target is not a direct child of literal F:\.cache"
    }
    if ($path -ieq (Join-Path $root "cw")) {
        throw "fixture cleanup must never target canonical F:\.cache\cw"
    }
    return $path
}

function Remove-OwnedFixtureTree {
    param([System.IO.DirectoryInfo]$Directory)

    $Directory.Attributes = [System.IO.FileAttributes]::Normal
    foreach ($child in @(Get-ChildItem -LiteralPath $Directory.FullName -Force)) {
        if ($child -is [System.IO.DirectoryInfo]) {
            if (($child.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
                [System.IO.Directory]::Delete($child.FullName)
            } else {
                $child.Attributes = [System.IO.FileAttributes]::Normal
                Remove-OwnedFixtureTree $child
            }
        } else {
            if (($child.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0) {
                [System.IO.File]::SetAttributes($child.FullName, [System.IO.FileAttributes]::Normal)
            }
            [System.IO.File]::Delete($child.FullName)
        }
    }
    [System.IO.Directory]::Delete($Directory.FullName)
}

$namespaceNames = Get-FixtureNames $env:CARGO_VALIDATE_WINDOWS_TEST_FIXTURE_CLEANUP_NAMESPACES "namespace"
$tempNames = Get-FixtureNames $env:CARGO_VALIDATE_WINDOWS_TEST_FIXTURE_CLEANUP_TEMP_LEAVES "temp leaf"
$paths = [System.Collections.Generic.List[string]]::new()
foreach ($name in $namespaceNames) {
    $paths.Add((Get-OwnedFixturePath $name "namespace"))
}
foreach ($name in $tempNames) {
    $paths.Add((Get-OwnedFixturePath $name "temp leaf"))
}

if ($action -eq "assert-absent") {
    foreach ($path in $paths) {
        if (Test-Path -LiteralPath $path) {
            throw "fixture cleanup expected an absent path: $path"
        }
    }
    exit 0
}
if ($action -eq "assert-present") {
    foreach ($path in $paths) {
        if (-not (Test-Path -LiteralPath $path)) {
            throw "fixture cleanup expected a present path: $path"
        }
    }
    exit 0
}

$mutex = [System.Threading.Mutex]::new($false, "Local\Cooldex.WindowsBuildCacheCleanup.v1")
$held = $false
try {
    if (-not $mutex.WaitOne(0)) {
        throw "fixture cleanup refused while the native validation mutex is busy"
    }
    $held = $true
    foreach ($path in $paths) {
        if (-not (Test-Path -LiteralPath $path)) {
            continue
        }
        $item = Get-Item -LiteralPath $path -Force
        if (
            $item -isnot [System.IO.DirectoryInfo] -or
            ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0
        ) {
            throw "fixture cleanup target must be a non-reparse directory: $path"
        }
        Remove-OwnedFixtureTree $item
        if (Test-Path -LiteralPath $path) {
            throw "fixture cleanup did not remove its exact target: $path"
        }
    }
} finally {
    if ($held) {
        $mutex.ReleaseMutex()
    }
    $mutex.Dispose()
}
"""

NATIVE_CACHE_DIRECT_CHILD_COUNT_COMMAND = r"""
$root = "F:\.cache"
if (Test-Path -LiteralPath $root) {
    [Console]::Out.Write((@(Get-ChildItem -LiteralPath $root -Force)).Count)
} else {
    [Console]::Out.Write("0")
}
"""


class HarnessPrerequisiteTests(unittest.TestCase):
    def test_direct_harness_fails_without_powershell(self) -> None:
        process = subprocess.run(
            [
                sys.executable,
                "-c",
                "import runpy\n"
                "from unittest import mock\n"
                "with mock.patch('pathlib.Path.is_file', return_value=False):\n"
                f"    runpy.run_path({str(Path(__file__).resolve())!r}, run_name='__main__')\n",
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertNotEqual(process.returncode, 0)
        self.assertIn(f"PowerShell 7 executable is required: {PWSH}", process.stderr)


class CargoValidateWindowsTests(unittest.TestCase):
    @staticmethod
    def native_cache_direct_child_count() -> int:
        process = subprocess.run(
            [
                PWSH,
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                NATIVE_CACHE_DIRECT_CHILD_COUNT_COMMAND,
            ],
            text=True,
            encoding="utf-8",
            errors="replace",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if process.returncode != 0:
            raise RuntimeError(
                "could not count direct F:\\cache children for fixture-lifetime "
                f"coverage: {process.stderr}"
            )
        try:
            return int(process.stdout)
        except ValueError as error:
            raise RuntimeError(
                "native F:\\cache direct-child count was not an integer: "
                f"{process.stdout!r}"
            ) from error

    @classmethod
    def setUpClass(cls) -> None:
        super().setUpClass()
        cls.direct_cache_child_count_before = cls.native_cache_direct_child_count()

    @classmethod
    def tearDownClass(cls) -> None:
        try:
            direct_cache_child_count_after = cls.native_cache_direct_child_count()
            if direct_cache_child_count_after != cls.direct_cache_child_count_before:
                raise AssertionError(
                    "the Windows harness changed the direct-child count below F:\\cache: "
                    f"before={cls.direct_cache_child_count_before}, "
                    f"after={direct_cache_child_count_after}"
                )
        finally:
            super().tearDownClass()

    def setUp(self) -> None:
        self.scratch_parent = TEST_SCRATCH_PARENT
        self.scratch_parent.mkdir(parents=True, exist_ok=True)
        self.scratch_parent = self.scratch_parent.resolve()
        self.temp_dir = tempfile.TemporaryDirectory(
            prefix="run.", dir=self.scratch_parent
        )
        self.temp_path = Path(self.temp_dir.name).resolve()
        self.assertTrue(
            self.temp_path.is_relative_to(self.scratch_parent),
            msg=f"temporary test directory escaped task cache parent: {self.temp_path}",
        )
        self.fixture_token = uuid.uuid4().hex
        self._namespace_sequence = 0
        self._temp_sequence = 0
        self._registered_namespaces: set[str] = set()
        self._registered_temp_leaves: set[str] = set()
        self._owned_namespaces: set[str] = set()
        self._owned_temp_leaves: set[str] = set()
        self._unadopted_namespaces: set[str] = set()
        self.addCleanup(self._assert_owned_fixture_paths_absent)
        self.addCleanup(self._cleanup_owned_fixture_paths)

    def tearDown(self) -> None:
        self.temp_dir.cleanup()

    def _is_current_case_namespace(self, namespace: str) -> bool:
        prefix = f"{FIXTURE_NAMESPACE_PREFIX}{self.fixture_token}-"
        suffix = namespace.removeprefix(prefix)
        return (
            namespace.startswith(prefix)
            and len(suffix) == 8
            and all(character in "0123456789abcdef" for character in suffix)
        )

    def _is_current_case_temp_leaf(self, temp_leaf: str) -> bool:
        prefix = f"p{self.fixture_token}"
        suffix = temp_leaf.removeprefix(prefix)
        return (
            temp_leaf.startswith(prefix)
            and len(suffix) == 8
            and all(character in "0123456789abcdef" for character in suffix)
        )

    def _new_owned_namespace(self) -> str:
        self._namespace_sequence += 1
        namespace = (
            f"{FIXTURE_NAMESPACE_PREFIX}{self.fixture_token}-"
            f"{self._namespace_sequence:08x}"
        )
        self._registered_namespaces.add(namespace)
        self._unadopted_namespaces.add(namespace)
        return namespace

    def _register_owned_namespace(self, namespace: str) -> None:
        self.assertTrue(
            self._is_current_case_namespace(namespace),
            msg=f"fixture namespace is not bound to the current test case: {namespace}",
        )
        if namespace not in self._registered_namespaces:
            self._registered_namespaces.add(namespace)
            self._unadopted_namespaces.add(namespace)

    def _new_owned_temp_leaf(self) -> str:
        self._temp_sequence += 1
        temp_leaf = f"p{self.fixture_token}{self._temp_sequence:08x}"
        self.assertTrue(self._is_current_case_temp_leaf(temp_leaf))
        self._registered_temp_leaves.add(temp_leaf)
        return temp_leaf

    def _native_fixture_lifetime(
        self,
        action: str,
        namespaces: set[str] | tuple[str, ...] | list[str],
        temp_leaves: set[str] | tuple[str, ...] | list[str],
        *,
        token: str | None = None,
    ) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        lifetime_names = {
            "CARGO_VALIDATE_WINDOWS_TEST_FIXTURE_CLEANUP_ACTION",
            "CARGO_VALIDATE_WINDOWS_TEST_FIXTURE_CLEANUP_TOKEN",
            "CARGO_VALIDATE_WINDOWS_TEST_FIXTURE_CLEANUP_NAMESPACES",
            "CARGO_VALIDATE_WINDOWS_TEST_FIXTURE_CLEANUP_TEMP_LEAVES",
        }
        wslenv_entries = [
            entry
            for entry in environment.get("WSLENV", "").split(":")
            if entry and entry.split("/", 1)[0] not in lifetime_names
        ]
        for name in lifetime_names:
            environment.pop(name, None)
        environment.update(
            {
                "CARGO_VALIDATE_WINDOWS_TEST_FIXTURE_CLEANUP_ACTION": action,
                "CARGO_VALIDATE_WINDOWS_TEST_FIXTURE_CLEANUP_TOKEN": token
                or self.fixture_token,
                "CARGO_VALIDATE_WINDOWS_TEST_FIXTURE_CLEANUP_NAMESPACES": json.dumps(
                    sorted(namespaces)
                ),
                "CARGO_VALIDATE_WINDOWS_TEST_FIXTURE_CLEANUP_TEMP_LEAVES": json.dumps(
                    sorted(temp_leaves)
                ),
            }
        )
        wslenv_entries.extend(lifetime_names)
        environment["WSLENV"] = ":".join(wslenv_entries)
        return subprocess.run(
            [
                PWSH,
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                NATIVE_FIXTURE_PATH_LIFETIME_COMMAND,
            ],
            text=True,
            encoding="utf-8",
            errors="replace",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            env=environment,
        )

    def _assert_native_fixture_lifetime(
        self,
        action: str,
        namespaces: set[str] | tuple[str, ...] | list[str],
        temp_leaves: set[str] | tuple[str, ...] | list[str],
    ) -> None:
        process = self._native_fixture_lifetime(action, namespaces, temp_leaves)
        self.assertEqual(
            process.returncode,
            0,
            msg=(
                f"native fixture-lifetime {action} failed\n"
                f"stdout:\n{process.stdout}\nstderr:\n{process.stderr}"
            ),
        )

    def _cleanup_owned_fixture_paths(self) -> None:
        if not self._owned_namespaces and not self._owned_temp_leaves:
            return
        self._assert_native_fixture_lifetime(
            "cleanup", self._owned_namespaces, self._owned_temp_leaves
        )

    def _assert_owned_fixture_paths_absent(self) -> None:
        if not self._owned_namespaces and not self._owned_temp_leaves:
            return
        self._assert_native_fixture_lifetime(
            "assert-absent", self._owned_namespaces, self._owned_temp_leaves
        )

    def _prepare_fixture_path_ownership(
        self,
        manifest: dict[str, object],
        path_creation_failure: str | None,
        fixture_process_opt_in: bool,
    ) -> dict[str, str | None] | None:
        if not fixture_process_opt_in:
            self.assertIsNone(path_creation_failure)
            return None
        runtime = manifest.get("windows_runtime")
        if not isinstance(runtime, dict):
            self.assertIsNone(path_creation_failure)
            return None
        namespace = runtime.get("workflow_namespace")
        if not isinstance(namespace, str) or not self._is_current_case_namespace(
            namespace
        ):
            self.assertIsNone(path_creation_failure)
            return None
        self._register_owned_namespace(namespace)
        if namespace in self._unadopted_namespaces:
            self._assert_native_fixture_lifetime("assert-absent", {namespace}, set())
            self._unadopted_namespaces.remove(namespace)
            self._owned_namespaces.add(namespace)
        temp_leaf = self._new_owned_temp_leaf()
        self._assert_native_fixture_lifetime("assert-absent", set(), {temp_leaf})
        self._owned_temp_leaves.add(temp_leaf)
        return {
            "token": self.fixture_token,
            "namespace": namespace,
            "temp_leaf": temp_leaf,
            "path_creation_failure": path_creation_failure,
        }

    def windows_path(self, path: Path | str) -> str:
        process = subprocess.run(
            ["wslpath", "-w", str(path)],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(process.returncode, 0, msg=f"wslpath failed: {process.stderr}")
        return process.stdout.strip()

    def unix_path(self, windows_path: str) -> Path:
        process = subprocess.run(
            ["wslpath", "-u", windows_path],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(process.returncode, 0, msg=f"wslpath failed: {process.stderr}")
        return Path(process.stdout.strip())

    def git_bytes(
        self,
        repository: Path,
        *arguments: str,
        allowed: tuple[int, ...] = (0,),
    ) -> bytes:
        process = subprocess.run(
            ["git", *arguments],
            cwd=repository,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertIn(
            process.returncode,
            allowed,
            msg=(
                f"git {' '.join(arguments)} failed with {process.returncode}\n"
                f"stdout={process.stdout!r}\nstderr={process.stderr.decode(errors='replace')}"
            ),
        )
        return process.stdout

    def git_text(self, repository: Path, *arguments: str) -> str:
        return self.git_bytes(repository, *arguments).decode().strip()

    def digest(self, value: bytes) -> dict[str, object]:
        return {"length": len(value), "sha256": hashlib.sha256(value).hexdigest()}

    def source_snapshot(self, source: Path) -> dict[str, object]:
        merge = self.git_bytes(
            source, "rev-parse", "-q", "--verify", "MERGE_HEAD", allowed=(0, 1)
        )
        symbolic_head = self.git_bytes(
            source, "symbolic-ref", "-q", "HEAD", allowed=(0, 1)
        )
        return {
            "head": self.git_text(source, "rev-parse", "HEAD"),
            "symbolic_head": symbolic_head.decode().strip() if symbolic_head else None,
            "merge_head": merge.decode().strip() if merge else None,
            "index_tree": self.git_text(source, "write-tree"),
            "refs": self.digest(self.git_bytes(source, "show-ref", "--head")),
            "index_entries": self.digest(
                self.git_bytes(source, "ls-files", "-s", "-z")
            ),
            "status": self.digest(
                self.git_bytes(source, "status", "--porcelain=v2", "-z")
            ),
            "untracked": self.digest(
                self.git_bytes(
                    source, "ls-files", "--others", "--exclude-standard", "-z"
                )
            ),
            "ignored_untracked": self.digest(
                self.git_bytes(
                    source,
                    "ls-files",
                    "--others",
                    "--ignored",
                    "--exclude-standard",
                    "-z",
                )
            ),
        }

    def windows_runtime(
        self, source_root: Path | None = None, namespace: str | None = None
    ) -> dict[str, object]:
        source = (source_root or self.temp_path).resolve()
        namespace = namespace or self._new_owned_namespace()
        if self._is_current_case_namespace(namespace):
            self._register_owned_namespace(namespace)
        return {
            "cache_root": r"F:\.cache",
            "workflow_namespace": namespace,
            "target": "x86_64-pc-windows-msvc",
            "rust_toolchain": "1.95.0-x86_64-pc-windows-msvc",
            "nextest_version": "0.9.103",
            "nextest_url": "https://example.invalid/cargo-nextest.zip",
            "nextest_sha256": "1" * 64,
            "v8_version": "150.4.0",
            "v8_archive_url": "https://example.invalid/rusty-v8.lib.gz",
            "v8_archive_sha256": "2" * 64,
            "v8_binding_url": "https://example.invalid/src-binding.rs",
            "v8_binding_sha256": "3" * 64,
            "resource_contract": {
                "resource_profile": "windows_nextest",
                "cold_minimum_free_disk_gib": 120,
                "warm_minimum_free_disk_gib": 5,
                "minimum_available_memory_gib": 30,
                "cargo_build_jobs": 16,
                "nextest_test_threads": 8,
                "monitor": True,
                "abort_free_gib": 5,
                "abort_free_pct": 5,
            },
            "source_materialization": {
                "posix_repo_root": str(source),
                "wsl_distro_name": os.environ.get("WSL_DISTRO_NAME", "Ubuntu-24.04"),
            },
        }

    def command(
        self,
        argv: list[str] | None = None,
        *,
        env: dict[str, str] | None = None,
        classification: str = "platform-neutral-test",
        platform: str | None = "windows",
        executor: str | None = "powershell",
        profile: str | None = "windows_nextest",
        artifact_policy: str = "ephemeral-codex-exe",
    ) -> dict[str, object]:
        return {
            "argv": production_windows_workspace_argv() if argv is None else argv,
            "reason": "Windows manifest executor fixture",
            "kind": WINDOWS_WORKSPACE_COMMAND_NAME,
            "env": {} if env is None else env,
            "platform": platform,
            "executor": executor,
            "classification": classification,
            "resource_profile": profile,
            "artifact_policy": artifact_policy,
            "command_id": "c" * 64,
            "fingerprint": "f" * 64,
            "job_contract_digest": "e" * 64,
        }

    def exclusion(self, classification: str) -> dict[str, object]:
        return {
            "argv": [],
            "reason": f"{classification} fixture",
            "kind": "excluded",
            "env": {},
            "platform": None,
            "executor": None,
            "classification": classification,
            "resource_profile": None,
            "artifact_policy": "none",
            "command_id": "b" * 64,
        }

    def manifest(
        self,
        commands: list[dict[str, object]],
        *,
        candidate_identity: dict[str, str | None] | None = None,
        source_root: Path | None = None,
        namespace: str | None = None,
    ) -> dict[str, object]:
        return {
            "action": "plan",
            "stage": "validation",
            "mode": "standard",
            "changed_files": [],
            "selected_packages": [],
            "selected_surfaces": [],
            "flags": [],
            "warnings": [],
            "commands": commands,
            "manual": [],
            "receipt_dir": None,
            "telemetry_level": "full",
            "candidate_identity": candidate_identity
            or {"head": None, "merge_head": None, "index_tree": None},
            "windows_runtime": self.windows_runtime(source_root, namespace),
            "plan_id": "a" * 64,
            "validation_tooling_digest": "d" * 64,
        }

    def helper_environment(
        self,
        *,
        fixture_opt_in: bool,
        preflight_fixture: dict[str, str] | None,
        preflight_fixture_opt_in: bool,
        preflight_fail_stage: str | None,
        fake_create_ignored: bool,
        fake_create_untracked: bool,
        fake_create_insta_pending: bool,
        fake_long_running: bool,
        fake_root_exits_first: bool,
        fake_immediate_descendant: bool,
        fake_containment_terminate_failure: bool,
        fake_wait_for_readiness: bool,
        fake_suppress_readiness: bool,
        bootstrap_fixture: dict[str, str] | None,
        bootstrap_fixture_opt_in: bool,
        source_git_config: Path | None,
        path_ownership: dict[str, str | None] | None,
    ) -> dict[str, str]:
        if fake_create_ignored and not fixture_opt_in:
            raise AssertionError(
                "ignored-file fake behavior requires the fake-cargo process opt-in"
            )
        if fake_create_untracked and not fixture_opt_in:
            raise AssertionError(
                "untracked-file fake behavior requires the fake-cargo process opt-in"
            )
        if fake_create_insta_pending and not fixture_opt_in:
            raise AssertionError(
                "Insta-pending fake behavior requires the fake-cargo process opt-in"
            )
        if fake_long_running and not fixture_opt_in:
            raise AssertionError(
                "long-running fake behavior requires the fake-cargo process opt-in"
            )
        if fake_root_exits_first and not fixture_opt_in:
            raise AssertionError(
                "root-exits-first fake behavior requires the fake-cargo process opt-in"
            )
        if fake_immediate_descendant and not fixture_opt_in:
            raise AssertionError(
                "immediate-descendant fake behavior requires the fake-cargo process opt-in"
            )
        if fake_containment_terminate_failure and not fixture_opt_in:
            raise AssertionError(
                "containment termination failure requires the fake-cargo process opt-in"
            )
        if fake_wait_for_readiness and not fixture_opt_in:
            raise AssertionError(
                "fake readiness synchronization requires the fake-cargo process opt-in"
            )
        if fake_suppress_readiness and not fake_wait_for_readiness:
            raise AssertionError(
                "fake readiness suppression requires the fake readiness barrier"
            )
        if bootstrap_fixture is not None and not fixture_opt_in:
            raise AssertionError(
                "bootstrap fixtures require the fake-cargo process opt-in"
            )
        process_env = os.environ.copy()
        test_names = {
            TEST_ONLY_FAKE_CARGO_OPT_IN,
            TEST_ONLY_PREFLIGHT_FIXTURE_OPT_IN,
            TEST_ONLY_PREFLIGHT_FAIL_STAGE,
            "CARGO_VALIDATE_WINDOWS_TEST_DISK_FREE_SAMPLES",
            TEST_ONLY_FAKE_CREATE_IGNORED,
            TEST_ONLY_FAKE_CREATE_UNTRACKED,
            TEST_ONLY_FAKE_CREATE_INSTA_PENDING,
            TEST_ONLY_FAKE_LONG_RUNNING,
            TEST_ONLY_FAKE_ROOT_EXITS_FIRST,
            TEST_ONLY_FAKE_IMMEDIATE_DESCENDANT,
            TEST_ONLY_FAKE_CONTAINMENT_TERMINATE_FAILURE,
            TEST_ONLY_FAKE_WAIT_FOR_READINESS,
            TEST_ONLY_FAKE_SUPPRESS_READINESS,
            TEST_ONLY_PATH_OWNERSHIP,
            TEST_ONLY_BOOTSTRAP_FIXTURE_OPT_IN,
            *PREFLIGHT_FIXTURE_FIELDS,
            *BOOTSTRAP_FIXTURE_FIELDS,
        }
        source_git_config_names = (
            {"GIT_CONFIG_GLOBAL", "GIT_CONFIG_NOSYSTEM"}
            if source_git_config is not None
            else set()
        )
        wslenv_entries = [
            entry
            for entry in process_env.get("WSLENV", "").split(":")
            if entry
            and entry.split("/", 1)[0] not in test_names | source_git_config_names
        ]
        for name in test_names | source_git_config_names:
            process_env.pop(name, None)
        if fixture_opt_in:
            process_env[TEST_ONLY_FAKE_CARGO_OPT_IN] = "1"
            wslenv_entries.append(TEST_ONLY_FAKE_CARGO_OPT_IN)
        if preflight_fixture is not None:
            missing = set(PREFLIGHT_FIXTURE_FIELDS).difference(preflight_fixture)
            if missing:
                raise AssertionError(
                    f"preflight fixture is missing fields: {sorted(missing)}"
                )
            process_env.update(preflight_fixture)
            wslenv_entries.extend(PREFLIGHT_FIXTURE_FIELDS)
            if "CARGO_VALIDATE_WINDOWS_TEST_DISK_FREE_SAMPLES" in preflight_fixture:
                wslenv_entries.append("CARGO_VALIDATE_WINDOWS_TEST_DISK_FREE_SAMPLES")
            if preflight_fixture_opt_in:
                process_env[TEST_ONLY_PREFLIGHT_FIXTURE_OPT_IN] = "1"
                wslenv_entries.append(TEST_ONLY_PREFLIGHT_FIXTURE_OPT_IN)
        if preflight_fail_stage is not None:
            process_env[TEST_ONLY_PREFLIGHT_FAIL_STAGE] = preflight_fail_stage
            wslenv_entries.append(TEST_ONLY_PREFLIGHT_FAIL_STAGE)
        if fake_create_ignored:
            process_env[TEST_ONLY_FAKE_CREATE_IGNORED] = "1"
            wslenv_entries.append(TEST_ONLY_FAKE_CREATE_IGNORED)
        if fake_create_untracked:
            process_env[TEST_ONLY_FAKE_CREATE_UNTRACKED] = "1"
            wslenv_entries.append(TEST_ONLY_FAKE_CREATE_UNTRACKED)
        if fake_create_insta_pending:
            process_env[TEST_ONLY_FAKE_CREATE_INSTA_PENDING] = "1"
            wslenv_entries.append(TEST_ONLY_FAKE_CREATE_INSTA_PENDING)
        if fake_long_running:
            process_env[TEST_ONLY_FAKE_LONG_RUNNING] = "1"
            wslenv_entries.append(TEST_ONLY_FAKE_LONG_RUNNING)
        if fake_root_exits_first:
            process_env[TEST_ONLY_FAKE_ROOT_EXITS_FIRST] = "1"
            wslenv_entries.append(TEST_ONLY_FAKE_ROOT_EXITS_FIRST)
        if fake_immediate_descendant:
            process_env[TEST_ONLY_FAKE_IMMEDIATE_DESCENDANT] = "1"
            wslenv_entries.append(TEST_ONLY_FAKE_IMMEDIATE_DESCENDANT)
        if fake_containment_terminate_failure:
            process_env[TEST_ONLY_FAKE_CONTAINMENT_TERMINATE_FAILURE] = "1"
            wslenv_entries.append(TEST_ONLY_FAKE_CONTAINMENT_TERMINATE_FAILURE)
        if fake_wait_for_readiness:
            process_env[TEST_ONLY_FAKE_WAIT_FOR_READINESS] = "1"
            wslenv_entries.append(TEST_ONLY_FAKE_WAIT_FOR_READINESS)
        if fake_suppress_readiness:
            process_env[TEST_ONLY_FAKE_SUPPRESS_READINESS] = "1"
            wslenv_entries.append(TEST_ONLY_FAKE_SUPPRESS_READINESS)
        if path_ownership is not None:
            process_env[TEST_ONLY_PATH_OWNERSHIP] = json.dumps(path_ownership)
            wslenv_entries.append(TEST_ONLY_PATH_OWNERSHIP)
        if bootstrap_fixture is not None:
            missing = set(BOOTSTRAP_FIXTURE_FIELDS).difference(bootstrap_fixture)
            if missing:
                raise AssertionError(
                    f"bootstrap fixture is missing fields: {sorted(missing)}"
                )
            process_env.update(bootstrap_fixture)
            wslenv_entries.extend(BOOTSTRAP_FIXTURE_FIELDS)
            if bootstrap_fixture_opt_in:
                process_env[TEST_ONLY_BOOTSTRAP_FIXTURE_OPT_IN] = "1"
                wslenv_entries.append(TEST_ONLY_BOOTSTRAP_FIXTURE_OPT_IN)
        if source_git_config is not None:
            source_git_config = source_git_config.resolve()
            self.assertTrue(
                source_git_config.is_file(),
                msg=f"missing scoped source Git config: {source_git_config}",
            )
            self.assertTrue(
                source_git_config.is_relative_to(self.scratch_parent),
                msg=(
                    "scoped source Git config escaped the disposable test fixture: "
                    f"{source_git_config}"
                ),
            )
            process_env["GIT_CONFIG_GLOBAL"] = str(source_git_config)
            process_env["GIT_CONFIG_NOSYSTEM"] = "1"
            wslenv_entries.extend(("GIT_CONFIG_GLOBAL/p", "GIT_CONFIG_NOSYSTEM"))
        if wslenv_entries:
            process_env["WSLENV"] = ":".join(wslenv_entries)
        else:
            process_env.pop("WSLENV", None)
        return process_env

    def invoke_raw(
        self,
        manifest: dict[str, object],
        *,
        fixture_opt_in: bool = False,
        preflight_fixture: dict[str, str] | None = DEFAULT_PREFLIGHT_FIXTURE,
        preflight_fixture_opt_in: bool = True,
        preflight_fail_stage: str | None = None,
        fake_create_ignored: bool = False,
        fake_create_untracked: bool = False,
        fake_create_insta_pending: bool = False,
        fake_long_running: bool = False,
        fake_root_exits_first: bool = False,
        fake_immediate_descendant: bool = False,
        fake_containment_terminate_failure: bool = False,
        fake_wait_for_readiness: bool = False,
        fake_suppress_readiness: bool = False,
        bootstrap_fixture: dict[str, str] | None = None,
        bootstrap_fixture_opt_in: bool = True,
        source_git_config: Path | None = None,
        path_creation_failure: str | None = None,
    ) -> subprocess.CompletedProcess[str]:
        manifest_path = (
            self.temp_path / f"manifest-{len(list(self.temp_path.iterdir()))}.json"
        )
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        self.assertTrue(
            manifest_path.resolve().is_relative_to(self.scratch_parent),
            msg=f"temporary manifest escaped task cache parent: {manifest_path}",
        )
        path_ownership = self._prepare_fixture_path_ownership(
            manifest,
            path_creation_failure,
            preflight_fixture is not None and preflight_fixture_opt_in,
        )
        return subprocess.run(
            [
                PWSH,
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                self.windows_path(HELPER),
                "-Manifest",
                self.windows_path(manifest_path),
            ],
            cwd=REPO_ROOT,
            text=True,
            encoding="utf-8",
            errors="replace",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            env=self.helper_environment(
                fixture_opt_in=fixture_opt_in,
                preflight_fixture=preflight_fixture,
                preflight_fixture_opt_in=preflight_fixture_opt_in,
                preflight_fail_stage=preflight_fail_stage,
                fake_create_ignored=fake_create_ignored,
                fake_create_untracked=fake_create_untracked,
                fake_create_insta_pending=fake_create_insta_pending,
                fake_long_running=fake_long_running,
                fake_root_exits_first=fake_root_exits_first,
                fake_immediate_descendant=fake_immediate_descendant,
                fake_containment_terminate_failure=fake_containment_terminate_failure,
                fake_wait_for_readiness=fake_wait_for_readiness,
                fake_suppress_readiness=fake_suppress_readiness,
                bootstrap_fixture=bootstrap_fixture,
                bootstrap_fixture_opt_in=bootstrap_fixture_opt_in,
                source_git_config=source_git_config,
                path_ownership=path_ownership,
            ),
        )

    def invoke(
        self,
        manifest: dict[str, object],
        *,
        fixture_opt_in: bool = False,
        preflight_fixture: dict[str, str] | None = DEFAULT_PREFLIGHT_FIXTURE,
        preflight_fixture_opt_in: bool = True,
        preflight_fail_stage: str | None = None,
        fake_create_ignored: bool = False,
        fake_create_untracked: bool = False,
        fake_create_insta_pending: bool = False,
        fake_long_running: bool = False,
        fake_root_exits_first: bool = False,
        fake_immediate_descendant: bool = False,
        fake_containment_terminate_failure: bool = False,
        fake_wait_for_readiness: bool = False,
        fake_suppress_readiness: bool = False,
        bootstrap_fixture: dict[str, str] | None = None,
        bootstrap_fixture_opt_in: bool = True,
        source_git_config: Path | None = None,
        path_creation_failure: str | None = None,
    ) -> tuple[subprocess.CompletedProcess[str], dict[str, object], dict[str, object]]:
        process = self.invoke_raw(
            manifest,
            fixture_opt_in=fixture_opt_in,
            preflight_fixture=preflight_fixture,
            preflight_fixture_opt_in=preflight_fixture_opt_in,
            preflight_fail_stage=preflight_fail_stage,
            fake_create_ignored=fake_create_ignored,
            fake_create_untracked=fake_create_untracked,
            fake_create_insta_pending=fake_create_insta_pending,
            fake_long_running=fake_long_running,
            fake_root_exits_first=fake_root_exits_first,
            fake_immediate_descendant=fake_immediate_descendant,
            fake_containment_terminate_failure=fake_containment_terminate_failure,
            fake_wait_for_readiness=fake_wait_for_readiness,
            fake_suppress_readiness=fake_suppress_readiness,
            bootstrap_fixture=bootstrap_fixture,
            bootstrap_fixture_opt_in=bootstrap_fixture_opt_in,
            source_git_config=source_git_config,
            path_creation_failure=path_creation_failure,
        )
        lines = [line for line in process.stdout.splitlines() if line.strip()]
        self.assertTrue(
            lines,
            msg=f"helper did not print a result summary\nSTDOUT:\n{process.stdout}\nSTDERR:\n{process.stderr}",
        )
        summary = json.loads(lines[-1])
        result_path = self.unix_path(str(summary["result_path"]))
        self.assertTrue(
            result_path.is_file(), msg=f"missing F-drive result evidence: {result_path}"
        )
        result = json.loads(result_path.read_text(encoding="utf-8"))
        return process, summary, result

    def command_result(
        self, process: subprocess.CompletedProcess[str], result: dict[str, object]
    ) -> dict[str, object]:
        commands = result["command_results"]
        self.assertIsInstance(commands, list)
        self.assertTrue(
            commands,
            msg=(
                "expected a command result but the executor recorded none\n"
                f"result.error: {result.get('error')!r}\n"
                f"process.stderr:\n{process.stderr}"
            ),
        )
        command = commands[0]
        self.assertIsInstance(command, dict)
        return command

    def test_invoke_raw_replaces_oem_diagnostic_bytes(self) -> None:
        global PWSH

        fake_pwsh = self.temp_path / "fake-pwsh.py"
        fake_pwsh.write_text(
            "#!/usr/bin/env python3\n"
            "import sys\n"
            "sys.stdout.buffer.write(b'fixture stdout\\n')\n"
            "sys.stderr.buffer.write(b'CS0102: \\xa0localized diagnostic\\n')\n"
            "raise SystemExit(17)\n",
            encoding="utf-8",
        )
        fake_pwsh.chmod(fake_pwsh.stat().st_mode | stat.S_IXUSR)
        original_pwsh = PWSH
        PWSH = str(fake_pwsh)
        try:
            process = self.invoke_raw(self.manifest([]), preflight_fixture=None)
        finally:
            PWSH = original_pwsh

        self.assertEqual(process.returncode, 17)
        self.assertEqual(process.stdout, "fixture stdout\n")
        self.assertIn("CS0102", process.stderr)
        self.assertIn("\ufffdlocalized diagnostic", process.stderr)

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_fixture_lifetime_retains_evidence_until_case_cleanup(self) -> None:
        manifest = self.manifest([])
        runtime = manifest["windows_runtime"]
        self.assertIsInstance(runtime, dict)
        namespace = runtime["workflow_namespace"]
        self.assertIsInstance(namespace, str)
        process, summary, result = self.invoke(manifest, fixture_opt_in=True)

        self.assertEqual(process.returncode, 0, msg=process.stderr)
        self.assertEqual(summary["status"], "success")
        evidence_dir = self.unix_path(str(summary["evidence_dir"]))
        self.assertTrue((evidence_dir / "preflight.json").is_file())
        self.assertEqual(
            result["paths"]["executor_root"].rsplit("\\", 1)[-1], namespace
        )
        temp_leaf = result["paths"]["temp_dir"].rsplit("\\", 1)[-1]
        self.assertTrue(self._is_current_case_temp_leaf(temp_leaf))
        self.assertFalse(self.unix_path(result["paths"]["temp_dir"]).exists())
        self._assert_native_fixture_lifetime("assert-present", {namespace}, set())

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_fixture_lifetime_cleans_pre_summary_path_creation_failure(self) -> None:
        manifest = self.manifest([])
        runtime = manifest["windows_runtime"]
        self.assertIsInstance(runtime, dict)
        namespace = runtime["workflow_namespace"]
        self.assertIsInstance(namespace, str)
        process = self.invoke_raw(
            manifest,
            fixture_opt_in=True,
            path_creation_failure="after-workset-root",
        )

        self.assertEqual(process.returncode, 1)
        self.assertEqual(process.stdout.strip(), "")
        self.assertIn("after executor namespace initialization", process.stderr)
        self._assert_native_fixture_lifetime("assert-present", {namespace}, set())

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_fixture_lifetime_tracks_independent_manifest_paths(self) -> None:
        first_manifest = self.manifest([])
        second_manifest = self.manifest([])
        first_runtime = first_manifest["windows_runtime"]
        second_runtime = second_manifest["windows_runtime"]
        self.assertIsInstance(first_runtime, dict)
        self.assertIsInstance(second_runtime, dict)
        first_namespace = first_runtime["workflow_namespace"]
        second_namespace = second_runtime["workflow_namespace"]
        self.assertIsInstance(first_namespace, str)
        self.assertIsInstance(second_namespace, str)
        self.assertNotEqual(first_namespace, second_namespace)

        first_process, first_summary, first_result = self.invoke(
            first_manifest, fixture_opt_in=True
        )
        second_process, second_summary, second_result = self.invoke(
            second_manifest, fixture_opt_in=True
        )

        self.assertEqual(first_process.returncode, 0, msg=first_process.stderr)
        self.assertEqual(second_process.returncode, 0, msg=second_process.stderr)
        self.assertNotEqual(
            first_summary["evidence_dir"], second_summary["evidence_dir"]
        )
        self.assertEqual(self._owned_namespaces, {first_namespace, second_namespace})
        temp_leaves = {
            first_result["paths"]["temp_dir"].rsplit("\\", 1)[-1],
            second_result["paths"]["temp_dir"].rsplit("\\", 1)[-1],
        }
        self.assertEqual(len(temp_leaves), 2)
        self.assertEqual(temp_leaves, self._owned_temp_leaves)
        self._assert_native_fixture_lifetime(
            "assert-present", self._owned_namespaces, set()
        )

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_fixture_lifetime_rejects_unowned_and_canonical_paths(self) -> None:
        foreign_token = "f" * 32
        if foreign_token == self.fixture_token:
            foreign_token = "e" * 32
        foreign_namespace = f"{FIXTURE_NAMESPACE_PREFIX}{foreign_token}-00000001"
        foreign = self._native_fixture_lifetime("cleanup", {foreign_namespace}, set())
        canonical = self._native_fixture_lifetime("cleanup", {"cw"}, set())

        self.assertNotEqual(foreign.returncode, 0)
        self.assertIn("not bound to this test token", foreign.stderr)
        self.assertNotEqual(canonical.returncode, 0)
        self.assertIn("not bound to this test token", canonical.stderr)

    def assert_f_cache_path(self, value: str) -> None:
        self.assertTrue(value.lower().startswith("f:\\.cache\\"), value)

    def fixture_env(self, exit_code: int = 0) -> dict[str, str]:
        return {
            "CARGO_VALIDATE_WINDOWS_TEST_FIXTURE": "fake-cargo-v1",
            "CARGO_VALIDATE_WINDOWS_TEST_EXIT_CODE": str(exit_code),
        }

    def make_bootstrap_fixture(
        self,
        *,
        zip_variant: str = "valid",
        final_host: str = "release-assets.githubusercontent.com",
    ) -> tuple[dict[str, str], dict[str, str]]:
        fixture_root = (
            self.temp_path / f"bootstrap-input-{len(list(self.temp_path.iterdir()))}"
        )
        fixture_root.mkdir()
        nextest_zip = fixture_root / "cargo-nextest.zip"
        with zipfile.ZipFile(
            nextest_zip, "w", compression=zipfile.ZIP_DEFLATED
        ) as archive:
            if zip_variant == "valid":
                archive.writestr("cargo-nextest.exe", b"MZfake-nextest\n")
            elif zip_variant == "unsafe":
                archive.writestr("../cargo-nextest.exe", b"MZunsafe\n")
            elif zip_variant == "duplicate":
                archive.writestr("cargo-nextest.exe", b"MZfirst\n")
                archive.writestr("CARGO-NEXTEST.EXE", b"MZsecond\n")
            elif zip_variant == "missing":
                archive.writestr("not-nextest.exe", b"MZmissing\n")
            elif zip_variant == "symlink":
                symlink = zipfile.ZipInfo("cargo-nextest.exe")
                symlink.create_system = 3
                symlink.external_attr = (stat.S_IFLNK | 0o777) << 16
                archive.writestr(symlink, b"target")
            else:
                raise AssertionError(
                    f"unsupported bootstrap ZIP fixture variant: {zip_variant}"
                )
        v8_archive = fixture_root / "rusty-v8.lib.gz"
        v8_archive.write_bytes(b"fixture-v8-archive\n")
        v8_binding = fixture_root / "src-binding.rs"
        v8_binding.write_bytes(b"// fixture V8 binding\n")
        return (
            {
                "CARGO_VALIDATE_WINDOWS_TEST_BOOTSTRAP_NEXTEST_ZIP": self.windows_path(
                    nextest_zip
                ),
                "CARGO_VALIDATE_WINDOWS_TEST_BOOTSTRAP_V8_ARCHIVE": self.windows_path(
                    v8_archive
                ),
                "CARGO_VALIDATE_WINDOWS_TEST_BOOTSTRAP_V8_BINDING": self.windows_path(
                    v8_binding
                ),
                "CARGO_VALIDATE_WINDOWS_TEST_BOOTSTRAP_FINAL_HOST": final_host,
            },
            {
                "nextest_sha256": hashlib.sha256(nextest_zip.read_bytes()).hexdigest(),
                "v8_archive_sha256": hashlib.sha256(
                    v8_archive.read_bytes()
                ).hexdigest(),
                "v8_binding_sha256": hashlib.sha256(
                    v8_binding.read_bytes()
                ).hexdigest(),
            },
        )

    def configure_bootstrap_runtime(
        self, manifest: dict[str, object], digests: dict[str, str]
    ) -> None:
        runtime = manifest["windows_runtime"]
        self.assertIsInstance(runtime, dict)
        runtime.update(
            {
                "nextest_url": "https://github.com/cooldex-fixture/cargo-nextest.zip",
                "nextest_sha256": digests["nextest_sha256"],
                "v8_archive_url": "https://github.com/cooldex-fixture/rusty-v8.lib.gz",
                "v8_archive_sha256": digests["v8_archive_sha256"],
                "v8_binding_url": "https://github.com/cooldex-fixture/src-binding.rs",
                "v8_binding_sha256": digests["v8_binding_sha256"],
            }
        )

    def create_source_fixture(
        self,
        *,
        include_symlink: bool = True,
        include_shadow_cargo: bool = False,
        include_msvc_setup: bool = False,
        stage_index_change: bool = True,
        source_name: str = "source",
    ) -> tuple[Path, dict[str, str | None], dict[str, object]]:
        source = self.temp_path / source_name
        codex_rs = source / "codex-rs"
        codex_rs.mkdir(parents=True)
        (codex_rs / "normal.txt").write_text("initial\n", encoding="utf-8")
        executable = codex_rs / "executable.sh"
        executable.write_text("#!/bin/sh\necho fixture\n", encoding="utf-8")
        executable.chmod(executable.stat().st_mode | stat.S_IXUSR)
        (codex_rs / ".gitignore").write_text(".fixture-ignored\n", encoding="utf-8")
        if include_shadow_cargo:
            (codex_rs / "cargo.exe").write_text(
                "not a native executable\n", encoding="utf-8"
            )
        if include_symlink:
            os.symlink("normal.txt", codex_rs / "linked-normal")
        if include_msvc_setup:
            setup_source = (
                REPO_ROOT
                / ".github"
                / "actions"
                / "setup-msvc-env"
                / "setup-msvc-env.ps1"
            )
            self.assertTrue(
                setup_source.is_file(),
                msg=f"missing canonical setup script: {setup_source}",
            )
            setup_destination = (
                source / ".github" / "actions" / "setup-msvc-env" / "setup-msvc-env.ps1"
            )
            setup_destination.parent.mkdir(parents=True)
            shutil.copy2(setup_source, setup_destination)
        self.git_bytes(source, "init")
        self.git_bytes(source, "config", "user.email", "fixture@example.invalid")
        self.git_bytes(source, "config", "user.name", "Windows Fixture")
        self.git_bytes(source, "add", ".")
        self.git_bytes(source, "commit", "-m", "initial fixture")
        if stage_index_change:
            (codex_rs / "normal.txt").write_text(
                "staged indexed content\n", encoding="utf-8"
            )
            self.git_bytes(source, "add", "codex-rs/normal.txt")
        index_entries = self.git_bytes(source, "ls-files", "-s", "-z")
        if include_symlink:
            self.assertIn(
                b"120000 ", index_entries, msg="fixture must contain a staged symlink"
            )
        identity = {
            "head": self.git_text(source, "rev-parse", "HEAD"),
            "merge_head": None,
            "index_tree": self.git_text(source, "write-tree"),
        }
        return source, identity, self.source_snapshot(source)

    def candidate_identity_for(self, source: Path) -> dict[str, str | None]:
        merge = self.git_bytes(
            source, "rev-parse", "-q", "--verify", "MERGE_HEAD", allowed=(0, 1)
        )
        return {
            "head": self.git_text(source, "rev-parse", "HEAD"),
            "merge_head": merge.decode().strip() if merge else None,
            "index_tree": self.git_text(source, "write-tree"),
        }

    def native_process_is_alive(self, pid: int) -> bool:
        process = subprocess.run(
            [
                PWSH,
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                f"if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} exit 1",
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        return process.returncode == 0

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_fake_nextest_receives_direct_argv_and_runtime_f_state(self) -> None:
        fixture = self.command(env=self.fixture_env(), artifact_policy="none")
        wsl_command = self.command(
            argv=["./scripts/cargo-guard.sh", "cargo", "test"],
            env={"CARGO_GUARD_RESOURCE_PROFILE": "package_test"},
            classification="wsl-unix-test",
            platform="wsl",
            executor="cargo-guard",
            profile="package_test",
            artifact_policy="none",
        )
        process, summary, result = self.invoke(
            self.manifest(
                [
                    fixture,
                    wsl_command,
                    self.exclusion("windows-only-excluded"),
                    self.exclusion("macos-not-applicable"),
                ]
            ),
            fixture_opt_in=True,
        )

        self.assertEqual(process.returncode, 0, msg=process.stderr)
        self.assertEqual(
            set(summary),
            {"schema", "status", "exit_code", "evidence_dir", "result_path"},
        )
        self.assertEqual(summary["schema"], 1)
        self.assertEqual(summary["status"], "success")
        self.assertEqual(result["schema"], 2)
        self.assertEqual(
            result["resource_contract"],
            {
                "resource_profile": "windows_nextest",
                "cold_minimum_free_disk_gib": 120,
                "warm_minimum_free_disk_gib": 5,
                "minimum_available_memory_gib": 30,
                "cargo_build_jobs": 16,
                "nextest_test_threads": 8,
                "monitor": True,
                "abort_free_gib": 5,
                "abort_free_pct": 5,
            },
        )
        self.assertEqual(
            result["candidate_materialization"]["status"],
            "all-null-candidate-test-fixture-only",
        )
        evidence_dir = self.unix_path(str(summary["evidence_dir"]))
        preflight = json.loads(
            (evidence_dir / "preflight.json").read_text(encoding="utf-8")
        )
        self.assertEqual(preflight["status"], "accepted")
        self.assertEqual(
            preflight["native_mutex"]["name"],
            r"Local\Cooldex.WindowsBuildCacheCleanup.v1",
        )
        self.assertEqual(preflight["native_mutex"]["status"], "held")
        self.assertEqual(
            [check["stage"] for check in preflight["execution_preflight"]],
            [
                "before-cold-workset-initialization",
                "before-materialization-or-command",
                "before-approved-command-1",
            ],
        )
        self.assertTrue(
            all(
                check["status"] == "passed"
                for check in preflight["execution_preflight"]
            )
        )
        self.assertTrue(
            all(
                not check["yolo"]
                and not check["bypassed_free_disk_floor"]
                and not check["bypassed_available_memory_floor"]
                for check in preflight["execution_preflight"]
            )
        )
        self.assertEqual(
            {
                record["index"]: record["status"]
                if "status" in record
                else record["disposition"]
                for record in result["command_results"]
            },
            {1: "success", 2: "wsl-not-executed", 3: "excluded", 4: "excluded"},
        )
        self.assertEqual(result["paths"]["cache_root"], r"F:\.cache")
        for key, value in result["paths"].items():
            if key not in {"cache_root", "cache_mode"}:
                self.assert_f_cache_path(value)
        command_result = next(
            record for record in result["command_results"] if record["index"] == 1
        )
        self.assertFalse(command_result["command_preflight"]["yolo"])
        self.assertEqual(
            command_result["launch_file_name"], command_result["fake_tool_path"]
        )
        self.assert_f_cache_path(command_result["launch_file_name"])
        self.assertTrue(self.unix_path(command_result["fake_tool_path"]).is_file())
        expected_argv = production_windows_workspace_argv()
        self.assertEqual(command_result["launch_arguments"], expected_argv[1:])
        logged_argv = (
            self.unix_path(command_result["fake_argv_path"])
            .read_text(encoding="utf-8")
            .splitlines()
        )
        self.assertEqual(logged_argv, expected_argv[1:])
        self.assertEqual(expected_argv[-1], logged_argv[-1])
        fake_env = dict(
            line.split("=", 1)
            for line in self.unix_path(command_result["fake_env_path"])
            .read_text(encoding="utf-8")
            .splitlines()
        )
        self.assertEqual(fake_env["CARGO_BUILD_JOBS"], "16")
        expected_output_root = result["paths"]["target_dir"]
        self.assertEqual(
            (
                result["paths"]["build_dir"],
                fake_env["CARGO_TARGET_DIR"],
                fake_env["CARGO_BUILD_BUILD_DIR"],
            ),
            (expected_output_root, expected_output_root, expected_output_root),
        )
        self.assertEqual(fake_env["CARGO_CACHE_AUTO_CLEAN_FREQUENCY"], "never")
        self.assertEqual(fake_env["NEXTEST_TEST_THREADS"], "8")
        self.assertEqual(fake_env["RUST_MIN_STACK"], "8388608")
        self.assertEqual(fake_env["FORCE_COLOR"], "0")
        path_entries = [entry for entry in fake_env["PATH"].split(";") if entry]
        self.assertEqual(
            (path_entries[0], fake_env.get("PYTHONPYCACHEPREFIX")),
            (r"F:\codex-tools\bin", fake_env["TEMP"] + r"\python-pycache"),
        )
        self.assertTrue(
            self.unix_path(path_entries[1]).joinpath("true.exe").is_file(),
            msg="the child PATH must include Git usr\\bin with true.exe",
        )
        self.assertTrue(self.unix_path(command_result["fake_argv_path"]).is_file())
        self.assertTrue(self.unix_path(command_result["fake_env_path"]).is_file())
        for key in (
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_BUILD_DIR",
            "CARGO_HOME",
            "RUSTUP_HOME",
            "TEMP",
            "TMP",
            "PYTHONPYCACHEPREFIX",
        ):
            self.assert_f_cache_path(fake_env[key])
        telemetry = command_result["runtime_telemetry"]
        self.assertGreaterEqual(telemetry["monitor_sample_count"], 1)
        self.assertEqual(telemetry["minimum_free_disk_bytes"], 128 * 1024**3)
        self.assertGreaterEqual(telemetry["memory_sample_count"], 1)
        self.assertEqual(telemetry["minimum_available_memory_bytes"], 64 * 1024**3)

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_fake_exit_status_is_preserved_in_result_evidence(self) -> None:
        process, summary, result = self.invoke(
            self.manifest([self.command(env=self.fixture_env(17))]), fixture_opt_in=True
        )
        self.assertEqual(process.returncode, 17, msg=process.stderr)
        self.assertEqual(summary["exit_code"], 17)
        self.assertEqual(result["status"], "command-failed")
        self.assertEqual(result["command_results"][0]["exit_code"], 17)
        self.assertEqual(result["command_results"][0]["status"], "failed")

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_accepts_planner_owned_workspace_selection_argv(self) -> None:
        planner_argv = [
            "cargo",
            "nextest",
            "run",
            "--workspace",
            "--profile",
            "local",
            "--no-fail-fast",
            "--no-tests",
            "fail",
            "--exclude",
            "planner-owned-package-one",
            "--exclude",
            "planner-owned-package-two",
            "-E",
            "package(codex-cli)",
        ]
        process, summary, result = self.invoke(
            self.manifest([self.command(argv=planner_argv, env=self.fixture_env())]),
            fixture_opt_in=True,
        )

        self.assertEqual(process.returncode, 0, msg=process.stderr)
        self.assertEqual(summary["status"], "success")
        command_result = result["command_results"][0]
        self.assertEqual(command_result["argv"], planner_argv)
        self.assertEqual(command_result["launch_arguments"], planner_argv[1:])
        self.assertEqual(
            self.unix_path(command_result["fake_argv_path"])
            .read_text(encoding="utf-8")
            .splitlines(),
            planner_argv[1:],
        )

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_accepts_planner_owned_selected_package_argv(self) -> None:
        planner_argv = [
            "cargo",
            "nextest",
            "run",
            "--profile",
            "local",
            "--no-fail-fast",
            "--no-tests",
            "fail",
            "-p",
            "planner-owned-package-one",
            "-p",
            "planner-owned-package-two",
            "-E",
            "package(codex-cli)",
        ]
        process, summary, result = self.invoke(
            self.manifest([self.command(argv=planner_argv, env=self.fixture_env())]),
            fixture_opt_in=True,
        )

        self.assertEqual(process.returncode, 0, msg=process.stderr)
        self.assertEqual(summary["status"], "success")
        command_result = result["command_results"][0]
        self.assertEqual(command_result["argv"], planner_argv)
        self.assertEqual(command_result["launch_arguments"], planner_argv[1:])
        self.assertEqual(
            self.unix_path(command_result["fake_argv_path"])
            .read_text(encoding="utf-8")
            .splitlines(),
            planner_argv[1:],
        )

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_rejects_non_test_entries_before_creating_or_launching_fake_cargo(
        self,
    ) -> None:
        unsafe_argv = (
            ["cargo", "install", "codex-cli"],
            ["cargo", "package"],
            ["cargo", "release"],
            ["cargo", "generate"],
            ["cargo", "nextest", "list"],
            ["cargo", "test"],
            ["just", "test"],
            ["codex", "install"],
            ["cargo", "run"],
        )
        valid_fixture = self.command(env=self.fixture_env())
        for argv in unsafe_argv:
            with self.subTest(argv=argv):
                process, summary, result = self.invoke(
                    self.manifest([valid_fixture, self.command(argv=argv)]),
                    fixture_opt_in=True,
                )
                self.assertEqual(process.returncode, 1)
                self.assertEqual(summary["status"], "preflight-failed")
                self.assertEqual(result["command_results"], [])
                self.assertFalse(
                    self.unix_path(result["paths"]["tool_staging"])
                    .joinpath("command-1", "cargo.exe")
                    .exists(),
                    msg="a rejected manifest must not create or launch the fake tool",
                )

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_rejects_invalid_candidate_runtime_and_manifest_shape(self) -> None:
        invalid_candidate = self.manifest([self.command(env=self.fixture_env())])
        invalid_candidate["candidate_identity"] = {
            "head": "a" * 40,
            "merge_head": None,
            "index_tree": None,
        }
        process, summary, result = self.invoke(invalid_candidate, fixture_opt_in=True)
        self.assertEqual(process.returncode, 1)
        self.assertEqual(summary["status"], "preflight-failed")
        self.assertEqual(result["command_results"], [])

        for field in ("plan_id", "validation_tooling_digest"):
            with self.subTest(field=field):
                malformed = self.manifest([self.command(env=self.fixture_env())])
                malformed[field] = "not-a-sha256-digest"
                process = self.invoke_raw(malformed, fixture_opt_in=True)
                self.assertEqual(process.returncode, 1)
                self.assertEqual(process.stdout.strip(), "")
                self.assertIn(field, process.stderr)

        for mutation in (
            lambda value: value.__setitem__("unexpected", "value"),
            lambda value: value["windows_runtime"].__setitem__("unexpected", "value"),
            lambda value: value["windows_runtime"].pop("resource_contract"),
            lambda value: value["windows_runtime"].__setitem__(
                "reuse_run_root", r"F:\.cache\cw\workset"
            ),
        ):
            with self.subTest(mutation=mutation):
                malformed = self.manifest([self.command(env=self.fixture_env())])
                mutation(malformed)
                process = self.invoke_raw(malformed, fixture_opt_in=True)
                self.assertEqual(process.returncode, 1)
                self.assertEqual(process.stdout.strip(), "")
                self.assertTrue(process.stderr.strip())

        unsafe_env = self.manifest([self.command(env={"PATH": r"C:\unsafe"})])
        process, summary, result = self.invoke(unsafe_env)
        self.assertEqual(process.returncode, 1)
        self.assertEqual(summary["status"], "preflight-failed")
        self.assertEqual(result["command_results"], [])

        malformed_exclusion = self.exclusion("windows-only-excluded")
        malformed_exclusion["platform"] = "windows"
        process, summary, result = self.invoke(self.manifest([malformed_exclusion]))
        self.assertEqual(process.returncode, 1)
        self.assertEqual(summary["status"], "preflight-failed")
        self.assertEqual(result["command_results"], [])

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_rejects_malformed_command_integrity_before_fake_tool_creation(
        self,
    ) -> None:
        for field in ("command_id", "fingerprint", "job_contract_digest"):
            with self.subTest(field=field):
                command = self.command(env=self.fixture_env())
                command[field] = "not-a-sha256-digest"
                process, summary, result = self.invoke(
                    self.manifest([command]), fixture_opt_in=True
                )
                self.assertEqual(process.returncode, 1)
                self.assertEqual(summary["status"], "preflight-failed")
                self.assertEqual(result["command_results"], [])
                self.assertFalse(
                    self.unix_path(result["paths"]["tool_staging"])
                    .joinpath("command-1", "cargo.exe")
                    .exists(),
                    msg="a malformed integrity field must fail before fake tool creation",
                )

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_fake_cargo_requires_test_only_opt_in(self) -> None:
        manifest = self.manifest([self.command(env=self.fixture_env())])
        process, summary, result = self.invoke(manifest)
        self.assertEqual(process.returncode, 1)
        self.assertEqual(summary["status"], "preflight-failed")
        self.assertEqual(result["status"], "preflight-failed")
        self.assertIn("test-only process opt-in", str(result["error"]))
        self.assertFalse(
            self.unix_path(result["paths"]["tool_staging"])
            .joinpath("command-1", "cargo.exe")
            .exists(),
            msg="fixture without process opt-in must not create a fake tool",
        )

        process, summary, result = self.invoke(
            self.manifest([self.command(env=self.fixture_env())]), fixture_opt_in=True
        )
        self.assertEqual(process.returncode, 0, msg=process.stderr)
        self.assertEqual(summary["status"], "success")
        self.assertTrue(
            self.unix_path(result["paths"]["tool_staging"])
            .joinpath("command-1", "cargo.exe")
            .is_file(),
            msg="the all-null fixture route must run only with process opt-in",
        )

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_bound_fixture_materializes_exact_candidate_and_preserves_source(
        self,
    ) -> None:
        source, identity, before = self.create_source_fixture()
        process, summary, result = self.invoke(
            self.manifest(
                [self.command(env=self.fixture_env(), artifact_policy="none")],
                candidate_identity=identity,
                source_root=source,
            ),
            fixture_opt_in=True,
        )
        self.assertEqual(process.returncode, 0, msg=process.stderr)
        self.assertEqual(summary["status"], "success")
        evidence_dir = self.unix_path(str(summary["evidence_dir"]))
        preflight = json.loads(
            (evidence_dir / "preflight.json").read_text(encoding="utf-8")
        )
        materialization = result["candidate_materialization"]
        self.assertEqual(materialization["status"], "success")
        self.assertTrue(materialization["materialized"])
        self.assertEqual(preflight["native_git"], materialization["native_git"])
        self.assertTrue(
            materialization["native_git"]["path"].lower().startswith("c:\\")
        )
        self.assertRegex(materialization["native_git"]["sha256"], r"^[0-9a-f]{64}$")
        self.assertEqual(materialization["source_before"]["head"], identity["head"])
        self.assertEqual(
            materialization["source_before"]["index_tree"], identity["index_tree"]
        )
        self.assertEqual(materialization["source_after"]["head"], identity["head"])
        self.assertEqual(
            materialization["candidate_before_command"]["index_tree"],
            identity["index_tree"],
        )
        self.assertEqual(
            materialization["candidate_after_command"]["index_tree"],
            identity["index_tree"],
        )
        self.assertEqual(
            materialization["source_after_command_loop"]["symbolic_head"],
            before["symbolic_head"],
        )
        self.assertEqual(
            materialization["source_after_command_loop"]["index_tree"],
            identity["index_tree"],
        )
        self.assertEqual(before, self.source_snapshot(source))
        candidate = self.unix_path(result["paths"]["candidate_root"])
        candidate_codex = candidate / "codex-rs"
        self.assertEqual(
            (candidate_codex / "normal.txt").read_text(encoding="utf-8"),
            "staged indexed content\n",
        )
        self.assertTrue(
            (candidate_codex / "executable.sh").stat().st_mode & stat.S_IXUSR,
            msg="candidate executable mode was not preserved",
        )
        placeholder = candidate_codex / "linked-normal"
        self.assertTrue(placeholder.is_file())
        self.assertFalse(
            placeholder.is_symlink(),
            msg="Windows candidate must record the explicit non-reparse git-symlink placeholder route",
        )
        self.assertEqual(placeholder.read_bytes(), b"normal.txt")
        candidate_index_entries = self.git_bytes(candidate, "ls-files", "-s", "-z")
        self.assertIn(
            b"120000 ",
            candidate_index_entries,
            msg="candidate index must retain the source symlink mode",
        )
        placeholders = materialization["candidate_before_command"][
            "symlink_placeholders"
        ]
        self.assertEqual(len(placeholders), 1)
        self.assertEqual(placeholders[0]["disposition"], "git-symlink-placeholder")
        self.assertEqual(placeholders[0]["mode"], "120000")
        self.assertEqual(
            placeholders[0]["indexed_blob_sha256"],
            placeholders[0]["placeholder_sha256"],
        )
        self.assertEqual(
            materialization["candidate_after_command"]["symlink_placeholders"],
            placeholders,
        )
        command_result = result["command_results"][0]
        self.assertTrue(
            command_result["working_directory"].lower().endswith(r"\candidate\codex-rs")
        )
        expected_argv = production_windows_workspace_argv()
        self.assertEqual(
            self.unix_path(command_result["fake_argv_path"])
            .read_text(encoding="utf-8")
            .splitlines(),
            expected_argv[1:],
        )

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_bound_fixture_materializes_clean_index_candidate(self) -> None:
        source, identity, before = self.create_source_fixture(stage_index_change=False)
        self.assertEqual(
            self.git_bytes(source, "diff", "--cached", "--binary", "HEAD"), b""
        )
        process, summary, result = self.invoke(
            self.manifest(
                [self.command(env=self.fixture_env(), artifact_policy="none")],
                candidate_identity=identity,
                source_root=source,
            ),
            fixture_opt_in=True,
        )

        self.assertEqual(process.returncode, 0, msg=process.stderr)
        self.assertEqual(summary["status"], "success")
        materialization = result["candidate_materialization"]
        self.assertEqual(materialization["status"], "success")
        self.assertTrue(materialization["materialized"])
        self.assertEqual(
            self.unix_path(materialization["patch_path"]).read_bytes(), b""
        )
        self.assertEqual(
            materialization["candidate_before_command"]["index_tree"],
            identity["index_tree"],
        )
        self.assertEqual(
            materialization["candidate_after_command"]["index_tree"],
            identity["index_tree"],
        )
        self.assertEqual(before, self.source_snapshot(source))
        candidate = self.unix_path(result["paths"]["candidate_root"])
        self.assertEqual(
            (candidate / "codex-rs" / "normal.txt").read_text(encoding="utf-8"),
            "initial\n",
        )

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_bound_fixture_uses_scoped_wsl_source_excludes(self) -> None:
        source, identity, _ = self.create_source_fixture(include_symlink=False)
        ignored_name = "source-only-wsl-global-ignore.txt"
        (source / ignored_name).write_text(
            "ignored by scoped config\n", encoding="utf-8"
        )
        empty_scoped_git_config = self.temp_path / "empty-source-gitconfig"
        empty_scoped_git_config.write_text("", encoding="utf-8")
        isolated_git_environment = os.environ.copy()
        isolated_git_environment.update(
            {
                "GIT_CONFIG_GLOBAL": str(empty_scoped_git_config),
                "GIT_CONFIG_NOSYSTEM": "1",
            }
        )
        ordinary_untracked_process = subprocess.run(
            ["git", "ls-files", "--others", "--exclude-standard", "-z"],
            cwd=source,
            text=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            env=isolated_git_environment,
        )
        self.assertEqual(
            ordinary_untracked_process.returncode,
            0,
            msg=ordinary_untracked_process.stderr.decode(errors="replace"),
        )
        self.assertEqual(
            ordinary_untracked_process.stdout, f"{ignored_name}\0".encode()
        )

        excludes = self.temp_path / "source-excludes"
        excludes.write_text(f"{ignored_name}\n", encoding="utf-8")
        scoped_git_config = self.temp_path / "source-gitconfig"
        config_process = subprocess.run(
            [
                "git",
                "config",
                "--file",
                str(scoped_git_config),
                "core.excludesFile",
                str(excludes),
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(config_process.returncode, 0, msg=config_process.stderr)

        manifest = self.manifest(
            [self.command(env=self.fixture_env(), artifact_policy="none")],
            candidate_identity=identity,
            source_root=source,
        )
        process, summary, result = self.invoke(
            manifest,
            fixture_opt_in=True,
            source_git_config=empty_scoped_git_config,
        )
        self.assertEqual(process.returncode, 1)
        self.assertEqual(summary["status"], "preflight-failed")
        self.assertIn(
            "source repository has ordinary untracked paths", str(result["error"])
        )
        self.assertFalse(self.unix_path(result["paths"]["candidate_root"]).exists())

        process, summary, result = self.invoke(
            self.manifest(
                [self.command(env=self.fixture_env(), artifact_policy="none")],
                candidate_identity=identity,
                source_root=source,
            ),
            fixture_opt_in=True,
            source_git_config=scoped_git_config,
        )
        self.assertEqual(process.returncode, 0, msg=process.stderr)
        self.assertEqual(summary["status"], "success")
        materialization = result["candidate_materialization"]
        expected_ignored_length = len(ignored_name.encode()) + 1
        for source_snapshot in (
            materialization["source_before"],
            materialization["source_after"],
            materialization["source_after_command_loop"],
        ):
            self.assertEqual(source_snapshot["untracked"]["byte_length"], 0)
            self.assertEqual(
                source_snapshot["ignored_untracked"]["byte_length"],
                expected_ignored_length,
            )
        self.assertEqual(
            materialization["candidate_before_command"]["ignored_untracked"][
                "byte_length"
            ],
            0,
        )
        self.assertTrue((source / ignored_name).is_file())

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_bound_fixture_rechecks_candidate_after_fake_command_and_resets_per_command_state(
        self,
    ) -> None:
        source, identity, before = self.create_source_fixture(
            include_symlink=False, include_shadow_cargo=True
        )
        process, summary, result = self.invoke(
            self.manifest(
                [self.command(env=self.fixture_env(), artifact_policy="none")],
                candidate_identity=identity,
                source_root=source,
            ),
            fixture_opt_in=True,
        )
        self.assertEqual(process.returncode, 0, msg=process.stderr)
        self.assertEqual(summary["status"], "success")
        materialization = result["candidate_materialization"]
        self.assertEqual(materialization["status"], "success")
        self.assertEqual(
            materialization["candidate_before_command"]["index_tree"],
            identity["index_tree"],
        )
        self.assertEqual(
            materialization["candidate_after_command"]["index_tree"],
            identity["index_tree"],
        )
        self.assertEqual(before, self.source_snapshot(source))
        command_result = result["command_results"][0]
        self.assertEqual(command_result["status"], "success")
        self.assertTrue(
            command_result["working_directory"].lower().endswith(r"\candidate\codex-rs")
        )
        candidate = self.unix_path(result["paths"]["candidate_root"])
        self.assertTrue((candidate / "codex-rs" / "cargo.exe").is_file())
        self.assertEqual(
            command_result["launch_file_name"], command_result["fake_tool_path"]
        )
        self.assertTrue(
            command_result["fake_tool_path"].lower().startswith("f:\\.cache\\")
        )
        self.assertNotEqual(
            command_result["launch_file_name"].casefold(),
            self.windows_path(candidate / "codex-rs" / "cargo.exe").casefold(),
        )
        with self.subTest("second-command-preflight"):
            source, identity, before = self.create_source_fixture(
                include_symlink=False,
                source_name="second-command-preflight-source",
            )
            command = self.command(env=self.fixture_env(), artifact_policy="none")
            process, summary, result = self.invoke(
                self.manifest(
                    [command, command.copy()],
                    candidate_identity=identity,
                    source_root=source,
                ),
                fixture_opt_in=True,
                preflight_fail_stage="before-approved-command-2",
            )

            self.assertEqual(process.returncode, 1, msg=process.stderr)
            self.assertEqual(summary["status"], "preflight-failed")
            self.assertEqual(result["status"], "preflight-failed")
            self.assertIn("before-approved-command-2", str(result["error"]))
            self.assertEqual(
                [
                    (record["index"], record["status"])
                    for record in result["command_results"]
                ],
                [(1, "success")],
            )
            self.assertEqual(result["candidate_materialization"]["status"], "success")
            evidence_dir = self.unix_path(str(summary["evidence_dir"]))
            preflight = json.loads(
                (evidence_dir / "preflight.json").read_text(encoding="utf-8")
            )
            self.assertEqual(preflight["status"], "failed")
            self.assertEqual(
                preflight["candidate_materialization"]["status"], "success"
            )
            self.assertEqual(
                [check["status"] for check in preflight["execution_preflight"]],
                ["passed", "passed", "passed", "failed"],
            )
            self.assertEqual(before, self.source_snapshot(source))

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_bound_fixture_keeps_insta_pending_artifacts_under_command_evidence(
        self,
    ) -> None:
        source, identity, before = self.create_source_fixture(
            include_symlink=False,
            include_shadow_cargo=True,
        )
        process, summary, result = self.invoke(
            self.manifest(
                [
                    self.command(
                        env=self.fixture_env(101),
                        artifact_policy="none",
                    )
                ],
                candidate_identity=identity,
                source_root=source,
            ),
            fixture_opt_in=True,
            fake_create_insta_pending=True,
        )

        self.assertEqual(process.returncode, 101, msg=process.stderr)
        self.assertEqual(summary["status"], "command-failed")
        self.assertEqual(result["status"], "command-failed")
        command_result = result["command_results"][0]
        self.assertEqual(command_result["status"], "failed")
        self.assertEqual(command_result["exit_code"], 101)
        fake_env = dict(
            line.split("=", 1)
            for line in self.unix_path(command_result["fake_env_path"])
            .read_text(encoding="utf-8")
            .splitlines()
        )
        evidence_dir = self.unix_path(str(summary["evidence_dir"]))
        pending_dir = self.unix_path(fake_env["INSTA_PENDING_DIR"])
        self.assert_f_cache_path(fake_env["INSTA_PENDING_DIR"])
        self.assertEqual(fake_env["INSTA_UPDATE"], "new")
        self.assertEqual(
            command_result["insta_pending_dir"], fake_env["INSTA_PENDING_DIR"]
        )
        self.assertEqual(pending_dir, evidence_dir / "command-1.insta-pending")
        self.assertEqual(
            (pending_dir / "snapshots" / "fixture.snap.new").read_text(
                encoding="utf-8"
            ),
            "external pending fixture\n",
        )
        self.assertEqual(
            (pending_dir / "src" / "fixture.rs.pending-snap").read_text(
                encoding="utf-8"
            ),
            "inline pending fixture\n",
        )
        candidate = self.unix_path(result["paths"]["candidate_root"])
        self.assertEqual(
            self.git_bytes(
                candidate,
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
            ),
            b"",
        )
        self.assertFalse(
            (candidate / "codex-rs" / "snapshots" / "fixture.snap.new").exists()
        )
        self.assertFalse(
            (candidate / "codex-rs" / "src" / "fixture.rs.pending-snap").exists()
        )
        materialization = result["candidate_materialization"]
        self.assertEqual(materialization["status"], "success")
        self.assertEqual(
            materialization["candidate_after_command"]["index_tree"],
            identity["index_tree"],
        )
        self.assertEqual(before, self.source_snapshot(source))

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_bound_fixture_reports_post_command_integrity_failure(self) -> None:
        for command_exit, expected_exit, command_status in (
            (0, 1, "success"),
            (100, 100, "failed"),
        ):
            with self.subTest(command_exit=command_exit):
                source, identity, before = self.create_source_fixture(
                    include_symlink=False,
                    source_name=f"integrity-failure-{command_exit}",
                )
                process, summary, result = self.invoke(
                    self.manifest(
                        [
                            self.command(
                                env=self.fixture_env(command_exit),
                                artifact_policy="none",
                            )
                        ],
                        candidate_identity=identity,
                        source_root=source,
                    ),
                    fixture_opt_in=True,
                    fake_create_untracked=True,
                )

                self.assertEqual(process.returncode, expected_exit, msg=process.stderr)
                self.assertEqual(summary["status"], "command-failed")
                self.assertEqual(summary["exit_code"], expected_exit)
                self.assertEqual(result["status"], "command-failed")
                self.assertEqual(result["exit_code"], expected_exit)
                self.assertIn("candidate has untracked paths", str(result["error"]))
                command_result = result["command_results"][0]
                self.assertEqual(command_result["status"], command_status)
                self.assertEqual(command_result["exit_code"], command_exit)
                self.assertTrue(
                    self.unix_path(command_result["fake_argv_path"]).is_file()
                )
                materialization = result["candidate_materialization"]
                self.assertEqual(
                    materialization["status"], "post-command-integrity-failed"
                )
                self.assertIsNone(materialization["candidate_after_command"])
                evidence_dir = self.unix_path(str(summary["evidence_dir"]))
                preflight = json.loads(
                    (evidence_dir / "preflight.json").read_text(encoding="utf-8")
                )
                self.assertEqual(preflight["status"], "accepted")
                self.assertIsNone(preflight["error"])
                self.assertEqual(
                    preflight["candidate_materialization"]["status"],
                    "post-command-integrity-failed",
                )
                candidate = self.unix_path(result["paths"]["candidate_root"])
                self.assertTrue(
                    (candidate / "codex-rs" / "fixture-untracked.txt").is_file()
                )
                self.assertIn(
                    b"fixture-untracked.txt\0",
                    self.git_bytes(
                        candidate,
                        "ls-files",
                        "--others",
                        "--exclude-standard",
                        "-z",
                    ),
                )
                self.assertEqual(before, self.source_snapshot(source))

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_reuses_selected_working_set_with_fresh_evidence_and_index_sync(
        self,
    ) -> None:
        source, first_identity, _ = self.create_source_fixture(
            include_symlink=False, include_msvc_setup=True
        )
        staged_tail = source / "codex-rs" / "staged-tail.txt"
        staged_tail.write_text("persisted staged tail\n", encoding="utf-8")
        self.git_bytes(source, "add", "codex-rs/staged-tail.txt")
        first_identity = self.candidate_identity_for(source)
        fixture, digests = self.make_bootstrap_fixture()
        cold_manifest = self.manifest(
            [self.command(env=self.fixture_env(), artifact_policy="none")],
            candidate_identity=first_identity,
            source_root=source,
        )
        cold_runtime = cold_manifest["windows_runtime"]
        self.assertIsInstance(cold_runtime, dict)
        namespace = cold_runtime["workflow_namespace"]
        self.configure_bootstrap_runtime(cold_manifest, digests)
        cold_process, cold_summary, cold_result = self.invoke(
            cold_manifest,
            fixture_opt_in=True,
            bootstrap_fixture=fixture,
            fake_create_ignored=True,
        )
        self.assertEqual(cold_process.returncode, 0, msg=cold_process.stderr)
        cold_root = self.unix_path(str(cold_result["paths"]["run_root"]))
        cold_evidence = self.unix_path(str(cold_summary["evidence_dir"]))
        cold_result_path = self.unix_path(str(cold_summary["result_path"]))
        cold_result_bytes = cold_result_path.read_bytes()
        candidate = self.unix_path(str(cold_result["paths"]["candidate_root"]))
        cache_state = json.loads(
            self.unix_path(str(cold_result["paths"]["cache_state_path"])).read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(cache_state["schema"], 2)
        self.assertNotIn("resource_contract", cache_state)
        self.assertEqual(
            set(cache_state["candidate_materialization"]),
            {"status", "materialized", "candidate_root"},
        )
        unchanged = candidate / "codex-rs" / "executable.sh"
        unchanged_mtime_ns = unchanged.stat().st_mtime_ns
        candidate_tail = candidate / "codex-rs" / "staged-tail.txt"
        tail_mtime_ns = candidate_tail.stat().st_mtime_ns
        ignored_output = candidate / "codex-rs" / ".fixture-ignored"
        self.assertTrue(ignored_output.is_file())
        self.assertEqual(
            (candidate / "codex-rs" / "normal.txt").read_text(encoding="utf-8"),
            "staged indexed content\n",
        )

        incompatible_manifest = self.manifest(
            [self.command(env=self.fixture_env(), artifact_policy="none")],
            candidate_identity=first_identity,
            source_root=source,
            namespace=namespace,
        )
        self.configure_bootstrap_runtime(incompatible_manifest, digests)
        incompatible_runtime = incompatible_manifest["windows_runtime"]
        self.assertIsInstance(incompatible_runtime, dict)
        incompatible_runtime["nextest_sha256"] = "4" * 64
        incompatible_process, incompatible_summary, incompatible_result = self.invoke(
            incompatible_manifest, fixture_opt_in=True
        )
        self.assertEqual(incompatible_process.returncode, 1)
        self.assertEqual(incompatible_summary["status"], "preflight-failed")
        self.assertIn(
            "does not match the frozen manifest", incompatible_result["error"]
        )
        self.assertEqual(incompatible_result["command_results"], [])

        (source / "codex-rs" / "normal.txt").write_text(
            "warm indexed content\n", encoding="utf-8"
        )
        self.git_bytes(source, "add", "codex-rs/normal.txt")
        warm_identity = self.candidate_identity_for(source)
        warm_manifest = self.manifest(
            [self.command(env=self.fixture_env(), artifact_policy="none")],
            candidate_identity=warm_identity,
            source_root=source,
            namespace=namespace,
        )
        self.configure_bootstrap_runtime(warm_manifest, digests)
        warm_runtime = warm_manifest["windows_runtime"]
        self.assertIsInstance(warm_runtime, dict)
        warm_resource_contract = warm_runtime["resource_contract"]
        self.assertIsInstance(warm_resource_contract, dict)
        warm_resource_contract["abort_free_pct"] = 6
        warm_process, warm_summary, warm_result = self.invoke(
            warm_manifest, fixture_opt_in=True
        )

        self.assertEqual(warm_process.returncode, 0, msg=warm_process.stderr)
        self.assertEqual(warm_summary["status"], "success")
        self.assertEqual(
            warm_result["paths"]["run_root"], cold_result["paths"]["run_root"]
        )
        for name in (
            "candidate_root",
            "target_dir",
            "cargo_home",
            "rustup_home",
            "helper_state",
            "tool_staging",
            "v8_cache",
        ):
            self.assertEqual(warm_result["paths"][name], cold_result["paths"][name])
        warm_evidence = self.unix_path(str(warm_summary["evidence_dir"]))
        self.assertNotEqual(warm_evidence, cold_evidence)
        self.assertTrue((cold_evidence / "command-1.stdout.txt").is_file())
        self.assertTrue((warm_evidence / "command-1.stdout.txt").is_file())
        self.assertEqual(cold_result_path.read_bytes(), cold_result_bytes)
        self.assertEqual(
            (candidate / "codex-rs" / "normal.txt").read_text(encoding="utf-8"),
            "warm indexed content\n",
        )
        self.assertEqual(unchanged.stat().st_mtime_ns, unchanged_mtime_ns)
        self.assertEqual(candidate_tail.stat().st_mtime_ns, tail_mtime_ns)
        self.assertTrue(ignored_output.is_file())
        sync_patch = self.unix_path(
            str(warm_result["candidate_materialization"]["patch_path"])
        )
        self.assertIn(b"normal.txt", sync_patch.read_bytes())
        self.assertEqual(
            warm_result["candidate_materialization"]["candidate_after_command"][
                "index_tree"
            ],
            warm_identity["index_tree"],
        )
        self.assertEqual(warm_result["bootstrap"]["source"], "reused")
        self.assertEqual(
            warm_result["bootstrap"]["nextest"]["zip"]["path"],
            cold_result["bootstrap"]["nextest"]["zip"]["path"],
        )
        self.assertEqual(warm_result["resource_contract"]["abort_free_pct"], 6)
        warm_temp = str(warm_result["paths"]["temp_dir"])
        self.assertTrue(warm_temp.lower().startswith(r"f:\.cache\p"), warm_temp)
        self.assertNotIn("-", warm_temp)
        fake_env = dict(
            line.split("=", 1)
            for line in self.unix_path(
                warm_result["command_results"][0]["fake_env_path"]
            )
            .read_text(encoding="utf-8")
            .splitlines()
        )
        self.assertEqual(fake_env["TEMP"], warm_temp)
        self.assertEqual(fake_env["TMP"], warm_temp)
        self.assertEqual(fake_env["FORCE_COLOR"], "0")
        self.assertEqual(
            fake_env["PYTHONPYCACHEPREFIX"], warm_temp + r"\python-pycache"
        )

        self.git_bytes(
            source,
            "commit",
            "-m",
            "commit warm indexed content",
            "--",
            "codex-rs/normal.txt",
        )
        committed_identity = self.candidate_identity_for(source)
        committed_manifest = self.manifest(
            [self.command(env=self.fixture_env(), artifact_policy="none")],
            candidate_identity=committed_identity,
            source_root=source,
            namespace=namespace,
        )
        self.configure_bootstrap_runtime(committed_manifest, digests)
        committed_process, committed_summary, committed_result = self.invoke(
            committed_manifest, fixture_opt_in=True
        )
        self.assertEqual(committed_process.returncode, 0, msg=committed_process.stderr)
        self.assertEqual(committed_summary["status"], "success")
        self.assertTrue(
            committed_result["candidate_materialization"]["head_transition"]
        )
        self.assertEqual(
            committed_result["candidate_materialization"]["candidate_after_command"][
                "head"
            ],
            committed_identity["head"],
        )
        self.assertEqual(
            committed_result["paths"]["target_dir"], cold_result["paths"]["target_dir"]
        )
        self.assertEqual(unchanged.stat().st_mtime_ns, unchanged_mtime_ns)
        self.assertEqual(
            candidate_tail.read_text(encoding="utf-8"), "persisted staged tail\n"
        )
        self.assertEqual(candidate_tail.stat().st_mtime_ns, tail_mtime_ns)
        self.assertEqual(self._owned_namespaces, {namespace})
        self.assertEqual(len(self._owned_temp_leaves), 4)
        self._assert_native_fixture_lifetime(
            "assert-present", self._owned_namespaces, self._owned_temp_leaves
        )

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_bound_fixture_requires_opt_in_before_candidate_or_fake_creation(
        self,
    ) -> None:
        source, identity, before = self.create_source_fixture()
        process, summary, result = self.invoke(
            self.manifest(
                [self.command(env=self.fixture_env())],
                candidate_identity=identity,
                source_root=source,
            )
        )
        self.assertEqual(process.returncode, 1)
        self.assertEqual(summary["status"], "preflight-failed")
        self.assertIn("test-only process opt-in", str(result["error"]))
        self.assertFalse(self.unix_path(result["paths"]["candidate_root"]).exists())
        self.assertFalse(
            self.unix_path(result["paths"]["tool_staging"])
            .joinpath("command-1", "cargo.exe")
            .exists()
        )
        self.assertEqual(before, self.source_snapshot(source))

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_prepared_cache_and_live_output_survive_executor_interruption(self) -> None:
        source, identity, before = self.create_source_fixture(
            include_symlink=False, include_msvc_setup=True
        )
        fixture, digests = self.make_bootstrap_fixture()
        manifest = self.manifest(
            [self.command(env=self.fixture_env(), artifact_policy="none")],
            candidate_identity=identity,
            source_root=source,
        )
        self.configure_bootstrap_runtime(manifest, digests)
        namespace = manifest["windows_runtime"]["workflow_namespace"]
        executor_root = self.unix_path(rf"F:\.cache\{namespace}")
        manifest_path = self.temp_path / "manifest-interruption.json"
        manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
        path_ownership = self._prepare_fixture_path_ownership(manifest, None, True)
        environment = self.helper_environment(
            fixture_opt_in=True,
            preflight_fixture=DEFAULT_PREFLIGHT_FIXTURE,
            preflight_fixture_opt_in=True,
            preflight_fail_stage=None,
            fake_create_ignored=False,
            fake_create_untracked=False,
            fake_create_insta_pending=False,
            fake_long_running=True,
            fake_root_exits_first=False,
            fake_immediate_descendant=False,
            fake_containment_terminate_failure=False,
            fake_wait_for_readiness=True,
            fake_suppress_readiness=False,
            bootstrap_fixture=fixture,
            bootstrap_fixture_opt_in=True,
            source_git_config=None,
            path_ownership=path_ownership,
        )
        helper = self.windows_path(HELPER).replace("'", "''")
        native_manifest = self.windows_path(manifest_path).replace("'", "''")
        process = subprocess.Popen(
            [
                PWSH,
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                "[Console]::Out.WriteLine($PID); [Console]::Out.Flush(); "
                f"& '{helper}' -Manifest '{native_manifest}'",
            ],
            cwd=REPO_ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,
            env=environment,
        )
        self.assertIsNotNone(process.stdout)
        self.assertIsNotNone(process.stderr)
        native_pid = int(process.stdout.readline().strip())

        def stop_executor() -> None:
            stopped = subprocess.run(
                [
                    PWSH,
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    f"Stop-Process -Id {native_pid} -Force -ErrorAction Stop",
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            self.assertEqual(stopped.returncode, 0, msg=stopped.stderr)

        samples = {
            "stdout": "fixture stdout: café".encode(),
            "stderr": "fixture stderr: π".encode(),
        }
        observed = {name: bytearray() for name in samples}
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ, "stdout")
                selector.register(process.stderr, selectors.EVENT_READ, "stderr")
                deadline = time.monotonic() + 90
                while any(samples[name] not in observed[name] for name in samples):
                    remaining = deadline - time.monotonic()
                    self.assertGreater(remaining, 0, msg=observed)
                    events = selector.select(timeout=remaining)
                    self.assertTrue(events, msg=observed)
                    for key, _ in events:
                        chunk = os.read(key.fd, 4096)
                        if chunk:
                            observed[key.data].extend(chunk)
                        else:
                            selector.unregister(key.fileobj)
                    self.assertTrue(selector.get_map(), msg=observed)
            self.assertIsNone(process.poll(), "native output arrived only after exit")
            state_path = executor_root / "workset" / "cache-state.json"
            state_bytes = state_path.read_bytes()
            state = json.loads(state_bytes)
            self.assertEqual(state["status"], "ready")
            self.assertEqual(state["candidate_identity"], identity)
            self.assertEqual(state["bootstrap"]["status"], "success")
            self.assertEqual(
                state["candidate_materialization"],
                {
                    "status": "success",
                    "materialized": True,
                    "candidate_root": state["paths"]["candidate_root"],
                },
            )
            evidence_paths = list((executor_root / "r").glob("*/e"))
            self.assertEqual(len(evidence_paths), 1)
            cold_evidence = evidence_paths[0]
            child_pid = int((cold_evidence / "command-1.child-pid.txt").read_text())
            self.assertTrue(self.native_process_is_alive(child_pid))
            self.assertFalse((cold_evidence / "result.json").exists())
            for name, sample in samples.items():
                self.assertEqual(
                    (cold_evidence / f"command-1.{name}.txt").read_bytes(), sample
                )
            stop_executor()
            remaining_stdout, remaining_stderr = process.communicate(timeout=15)
            self.assertNotEqual(process.returncode, 0)
            self.assertNotIn(b'"result_path"', observed["stdout"] + remaining_stdout)
            self.assertEqual(observed["stderr"] + remaining_stderr, samples["stderr"])
            self.assertFalse(self.native_process_is_alive(child_pid))
            self.assertFalse((cold_evidence / "result.json").exists())
            self.assertEqual(state_path.read_bytes(), state_bytes)
        finally:
            if process.poll() is None:
                stop_executor()
                process.communicate(timeout=15)
            process.stdout.close()
            process.stderr.close()

        warm_manifest = self.manifest(
            [self.command(env=self.fixture_env(), artifact_policy="none")],
            candidate_identity=identity,
            source_root=source,
            namespace=namespace,
        )
        self.configure_bootstrap_runtime(warm_manifest, digests)
        warm_process, warm_summary, warm_result = self.invoke(
            warm_manifest, fixture_opt_in=True
        )
        self.assertEqual(warm_process.returncode, 0, msg=warm_process.stderr)
        self.assertEqual(warm_summary["status"], "success")
        self.assertEqual(warm_result["bootstrap"]["source"], "reused")
        self.assertEqual(warm_result["paths"]["cache_mode"], "warm")
        self.assertNotEqual(self.unix_path(warm_summary["evidence_dir"]), cold_evidence)
        warm_command = self.command_result(warm_process, warm_result)
        self.assertEqual(warm_command["exit_code"], 0)
        self.assertTrue(
            warm_process.stdout.startswith(samples["stdout"].decode() + "\n")
        )
        self.assertEqual(warm_process.stderr, samples["stderr"].decode())
        for name, sample in samples.items():
            self.assertEqual(
                self.unix_path(warm_command[f"{name}_path"]).read_bytes(), sample
            )
            self.assertEqual(warm_command[f"{name}_byte_length"], len(sample))
            self.assertEqual(
                (cold_evidence / f"command-1.{name}.txt").read_bytes(), sample
            )
        self.assertEqual(before, self.source_snapshot(source))

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_failed_bound_command_publishes_ready_cache_for_warm_retry(self) -> None:
        source, identity, _ = self.create_source_fixture(
            include_symlink=False, include_msvc_setup=True
        )
        fixture, digests = self.make_bootstrap_fixture()
        cold_manifest = self.manifest(
            [self.command(env=self.fixture_env(17), artifact_policy="none")],
            candidate_identity=identity,
            source_root=source,
        )
        self.configure_bootstrap_runtime(cold_manifest, digests)
        cold_runtime = cold_manifest["windows_runtime"]
        self.assertIsInstance(cold_runtime, dict)
        namespace = cold_runtime["workflow_namespace"]
        cold_process, cold_summary, cold_result = self.invoke(
            cold_manifest,
            fixture_opt_in=True,
            bootstrap_fixture=fixture,
        )
        self.assertEqual(cold_process.returncode, 17, msg=cold_process.stderr)
        self.assertEqual(cold_summary["status"], "command-failed")
        self.assertTrue(
            self.unix_path(cold_result["paths"]["cache_state_path"]).is_file(),
            msg="a normally terminated failed command must leave a reusable ready cache",
        )

        warm_manifest = self.manifest(
            [self.command(env=self.fixture_env(), artifact_policy="none")],
            candidate_identity=identity,
            source_root=source,
            namespace=namespace,
        )
        self.configure_bootstrap_runtime(warm_manifest, digests)
        warm_process, warm_summary, warm_result = self.invoke(
            warm_manifest, fixture_opt_in=True
        )
        self.assertEqual(warm_process.returncode, 0, msg=warm_process.stderr)
        self.assertEqual(warm_summary["status"], "success")
        self.assertEqual(warm_result["bootstrap"]["source"], "reused")
        self.assertNotEqual(warm_summary["evidence_dir"], cold_summary["evidence_dir"])
        self.assertNotEqual(
            warm_result["paths"]["temp_dir"], cold_result["paths"]["temp_dir"]
        )
        for name in (
            "candidate_root",
            "target_dir",
            "cargo_home",
            "rustup_home",
            "helper_state",
            "tool_staging",
            "v8_cache",
        ):
            self.assertEqual(warm_result["paths"][name], cold_result["paths"][name])

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_warm_reuse_rejects_a_reparse_component_of_the_workset(self) -> None:
        source, identity, _ = self.create_source_fixture(
            include_symlink=False, include_msvc_setup=True
        )
        bootstrap_fixture, digests = self.make_bootstrap_fixture()
        cold_manifest = self.manifest(
            [self.command(env=self.fixture_env(), artifact_policy="none")],
            candidate_identity=identity,
            source_root=source,
        )
        self.configure_bootstrap_runtime(cold_manifest, digests)
        namespace = cold_manifest["windows_runtime"]["workflow_namespace"]
        cold_process, _, cold_result = self.invoke(
            cold_manifest,
            fixture_opt_in=True,
            bootstrap_fixture=bootstrap_fixture,
        )
        self.assertEqual(cold_process.returncode, 0, msg=cold_process.stderr)
        candidate = cold_result["paths"]["candidate_root"]
        junction_process = subprocess.run(
            [
                PWSH,
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                f"$path = [IO.Path]::GetFullPath('{candidate}'); $target = \"$path.real\"; Move-Item -LiteralPath $path -Destination $target; New-Item -ItemType Junction -Path $path -Target $target | Out-Null",
            ],
            text=True,
            encoding="utf-8",
            errors="replace",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(junction_process.returncode, 0, msg=junction_process.stderr)
        warm_manifest = self.manifest(
            [self.command(env=self.fixture_env(), artifact_policy="none")],
            candidate_identity=identity,
            source_root=source,
            namespace=namespace,
        )
        self.configure_bootstrap_runtime(warm_manifest, digests)
        warm_process = self.invoke_raw(warm_manifest, fixture_opt_in=True)
        self.assertEqual(warm_process.returncode, 1)
        self.assertEqual(warm_process.stdout.strip(), "")
        self.assertIn("reparse point", warm_process.stderr)

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_bound_fixture_bootstrap_prepares_pins_and_rejects_bad_inputs(self) -> None:
        source, identity, before = self.create_source_fixture(
            include_shadow_cargo=True, include_msvc_setup=True
        )
        fixture, digests = self.make_bootstrap_fixture()
        manifest = self.manifest(
            [self.command(env=self.fixture_env(), artifact_policy="none")],
            candidate_identity=identity,
            source_root=source,
        )
        self.configure_bootstrap_runtime(manifest, digests)
        process, summary, result = self.invoke(
            manifest,
            fixture_opt_in=True,
            bootstrap_fixture=fixture,
        )

        self.assertEqual(process.returncode, 0, msg=process.stderr)
        self.assertEqual(
            set(summary),
            {"schema", "status", "exit_code", "evidence_dir", "result_path"},
        )
        self.assertEqual(summary["schema"], 1)
        self.assertEqual(summary["status"], "success")
        self.assertEqual(result["schema"], 2)
        bootstrap = result["bootstrap"]
        self.assertEqual(bootstrap["status"], "success")
        self.assertEqual(bootstrap["source"], "test-only-fixture")
        evidence_dir = self.unix_path(str(summary["evidence_dir"]))
        preflight = json.loads(
            (evidence_dir / "preflight.json").read_text(encoding="utf-8")
        )
        self.assertEqual(preflight["schema"], 2)
        self.assertEqual(preflight["bootstrap"], bootstrap)
        self.assertEqual(preflight["direct_toolchain"], bootstrap["direct_toolchain"])
        direct = bootstrap["direct_toolchain"]
        for name in ("cargo", "rustc", "rustdoc"):
            self.assertTrue(
                direct[name]["path"].lower().startswith("c:\\"), direct[name]
            )
            self.assertRegex(direct[name]["sha256"], r"^[0-9a-f]{64}$")
        msvc = bootstrap["msvc_setup"]
        self.assertEqual(msvc["status"], "success")
        self.assertTrue(
            msvc["script_path"]
            .lower()
            .endswith(r"\candidate\.github\actions\setup-msvc-env\setup-msvc-env.ps1")
        )
        self.assertIn(
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER",
            msvc["environment"]["record_names"],
        )
        self.assertEqual(
            msvc["environment"]["linker_variable"],
            "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER",
        )
        self.assert_f_cache_path(msvc["environment"]["path"])
        self.assert_f_cache_path(msvc["stdout_path"])
        self.assert_f_cache_path(msvc["stderr_path"])

        nextest = bootstrap["nextest"]
        self.assertTrue(self.unix_path(nextest["zip"]["path"]).is_file())
        self.assertTrue(
            self.unix_path(nextest["executable"]["executable_path"]).is_file()
        )
        self.assertEqual(nextest["executable"]["entry_name"], "cargo-nextest.exe")
        self.assertFalse(
            self.unix_path(nextest["zip"]["path"] + ".partial").exists(),
            msg="validated Nextest ZIP must have been atomically finalized",
        )
        self.assertFalse(
            self.unix_path(
                nextest["executable"]["executable_path"] + ".partial"
            ).exists(),
            msg="validated Nextest executable must have been atomically finalized",
        )
        for artifact in (bootstrap["v8"]["archive"], bootstrap["v8"]["binding"]):
            self.assertTrue(self.unix_path(artifact["path"]).is_file())
            self.assertFalse(
                self.unix_path(artifact["path"] + ".partial").exists(),
                msg="validated V8 artifact must have been atomically finalized",
            )
            self.assertEqual(
                artifact["final_uri"].split("/", 3)[2],
                "release-assets.githubusercontent.com",
            )

        command_result = result["command_results"][0]
        self.assertEqual(command_result["launch_kind"], "test-only-fake-cargo")
        self.assertEqual(
            command_result["launch_file_name"], command_result["fake_tool_path"]
        )
        self.assertNotEqual(
            command_result["launch_file_name"].casefold(),
            self.windows_path(source / "codex-rs" / "cargo.exe").casefold(),
        )
        fake_env = dict(
            line.split("=", 1)
            for line in self.unix_path(command_result["fake_env_path"])
            .read_text(encoding="utf-8")
            .splitlines()
        )
        path_entries = [entry for entry in fake_env["PATH"].split(";") if entry]
        self.assertGreaterEqual(len(path_entries), 4)
        self.assertEqual(path_entries[0], r"F:\codex-tools\bin")
        self.assertTrue(self.unix_path(path_entries[1]).joinpath("true.exe").is_file())
        self.assertEqual(path_entries[2:4], bootstrap["path_prefix"])
        self.assertEqual(
            fake_env["PYTHONPYCACHEPREFIX"],
            fake_env["TEMP"] + r"\python-pycache",
        )
        self.assert_f_cache_path(fake_env["PYTHONPYCACHEPREFIX"])
        self.assertEqual(
            fake_env["RUSTY_V8_ARCHIVE"], bootstrap["v8"]["archive"]["path"]
        )
        self.assertEqual(
            fake_env["RUSTY_V8_SRC_BINDING_PATH"], bootstrap["v8"]["binding"]["path"]
        )
        self.assertEqual(fake_env["RUSTY_V8_MIRROR"], "")
        self.assertEqual(fake_env["V8_FROM_SOURCE"], "")
        self.assertEqual(before, self.source_snapshot(source))

        missing_fixture_manifest = self.manifest(
            [self.command(env=self.fixture_env(), artifact_policy="none")],
            candidate_identity=identity,
            source_root=source,
        )
        self.configure_bootstrap_runtime(missing_fixture_manifest, digests)
        process, summary, result = self.invoke(
            missing_fixture_manifest,
            fixture_opt_in=True,
            bootstrap_fixture=fixture,
            bootstrap_fixture_opt_in=False,
        )
        self.assertEqual(process.returncode, 1)
        self.assertEqual(summary["status"], "preflight-failed")
        self.assertIn("bootstrap fixture data requires", str(result["error"]))
        self.assertFalse(self.unix_path(result["paths"]["candidate_root"]).exists())

        production_manifest = self.manifest(
            [self.command(artifact_policy="none")],
            candidate_identity=identity,
            source_root=source,
        )
        self.configure_bootstrap_runtime(production_manifest, digests)
        process, summary, result = self.invoke(
            production_manifest,
            fixture_opt_in=True,
            bootstrap_fixture=fixture,
        )
        self.assertEqual(process.returncode, 1)
        self.assertEqual(summary["status"], "preflight-failed")
        self.assertIn("cannot enable a production", str(result["error"]))
        self.assertFalse(self.unix_path(result["paths"]["candidate_root"]).exists())
        self.assertFalse(
            self.unix_path(result["paths"]["tool_staging"])
            .joinpath("command-1", "cargo.exe")
            .exists(),
            msg="bootstrap fixtures must not enable a production Cargo command",
        )

        failure_cases = (
            (
                "wrong-digest",
                "valid",
                "release-assets.githubusercontent.com",
                "SHA-256",
            ),
            ("unexpected-host", "valid", "fixture.example", "final URI"),
            (
                "unsafe-zip",
                "unsafe",
                "release-assets.githubusercontent.com",
                "unsafe entry",
            ),
            (
                "duplicate-zip",
                "duplicate",
                "release-assets.githubusercontent.com",
                "duplicate Windows-equivalent",
            ),
            (
                "missing-nextest",
                "missing",
                "release-assets.githubusercontent.com",
                "exactly one cargo-nextest.exe",
            ),
            (
                "symlink-nextest",
                "symlink",
                "release-assets.githubusercontent.com",
                "symbolic-link",
            ),
        )
        for label, zip_variant, final_host, expected in failure_cases:
            with self.subTest(label=label):
                case_fixture, case_digests = self.make_bootstrap_fixture(
                    zip_variant=zip_variant, final_host=final_host
                )
                case_manifest = self.manifest(
                    [self.command(env=self.fixture_env(), artifact_policy="none")],
                    candidate_identity=identity,
                    source_root=source,
                )
                self.configure_bootstrap_runtime(case_manifest, case_digests)
                if label == "wrong-digest":
                    case_manifest["windows_runtime"]["nextest_sha256"] = "0" * 64
                process, summary, result = self.invoke(
                    case_manifest,
                    fixture_opt_in=True,
                    bootstrap_fixture=case_fixture,
                )
                self.assertEqual(process.returncode, 1)
                self.assertEqual(summary["status"], "preflight-failed")
                self.assertEqual(result["bootstrap"]["status"], "failed")
                self.assertIn(expected.casefold(), str(result["error"]).casefold())
                self.assertEqual(result["command_results"], [])
                self.assertFalse(
                    self.unix_path(result["paths"]["tool_staging"])
                    .joinpath("command-1", "cargo.exe")
                    .exists(),
                    msg="bootstrap rejection must happen before fake-cargo creation",
                )
                self.assertEqual(before, self.source_snapshot(source))

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_rejects_reserved_or_overlong_namespace_before_run_creation(self) -> None:
        for namespace in ("CON", "n" * 65, "cw.", "cw..x"):
            with self.subTest(namespace=namespace):
                manifest = self.manifest([self.command(env=self.fixture_env())])
                manifest["windows_runtime"]["workflow_namespace"] = namespace
                process = self.invoke_raw(manifest, fixture_opt_in=True)
                self.assertEqual(process.returncode, 1)
                self.assertEqual(
                    process.stdout.strip(),
                    "",
                    msg="a rejected namespace must not create an F-drive run or summary",
                )
                self.assertIn("safe single namespace", process.stderr)

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_residual_cache_without_canonical_workset_fails_without_cold_fallback(
        self,
    ) -> None:
        process = self.invoke_raw(
            self.manifest([self.command(env=self.fixture_env())]),
            fixture_opt_in=True,
            preflight_fixture={
                **DEFAULT_PREFLIGHT_FIXTURE,
                "CARGO_VALIDATE_WINDOWS_TEST_CACHE_ROOT_STATE": "residual",
            },
        )
        self.assertEqual(process.returncode, 1)
        self.assertEqual(process.stdout.strip(), "")
        self.assertIn("not operator-cleared", process.stderr)

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_cold_resource_rejection_keeps_the_canonical_workset_absent(self) -> None:
        first_manifest = self.manifest([self.command(env=self.fixture_env())])
        namespace = first_manifest["windows_runtime"]["workflow_namespace"]
        process, summary, result = self.invoke(
            first_manifest,
            fixture_opt_in=True,
            preflight_fixture={
                **DEFAULT_PREFLIGHT_FIXTURE,
                "CARGO_VALIDATE_WINDOWS_TEST_DISK_FREE_BYTES": str(119 * 1024**3),
            },
        )
        self.assertEqual(process.returncode, 1)
        self.assertEqual(summary["status"], "preflight-failed")
        self.assertFalse(self.unix_path(result["paths"]["workset_root"]).exists())
        self.assertTrue(
            self.unix_path(summary["evidence_dir"]).joinpath("preflight.json").is_file()
        )
        retry_process, retry_summary, retry_result = self.invoke(
            self.manifest([self.command(env=self.fixture_env())], namespace=namespace),
            fixture_opt_in=True,
        )
        self.assertEqual(retry_process.returncode, 0, msg=retry_process.stderr)
        self.assertEqual(retry_summary["status"], "success")
        self.assertEqual(retry_result["paths"]["cache_mode"], "cold")

        malformed_manifest = self.manifest([self.command(env=self.fixture_env())])
        malformed_namespace = malformed_manifest["windows_runtime"][
            "workflow_namespace"
        ]
        malformed_process, _, malformed_result = self.invoke(
            malformed_manifest,
            fixture_opt_in=True,
            preflight_fixture={
                **DEFAULT_PREFLIGHT_FIXTURE,
                "CARGO_VALIDATE_WINDOWS_TEST_DISK_FREE_BYTES": str(119 * 1024**3),
            },
        )
        self.assertEqual(malformed_process.returncode, 1)
        run_root = malformed_result["paths"]["execution_root"]
        create_sibling = subprocess.run(
            [
                PWSH,
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                f"New-Item -ItemType Directory -Path '{run_root}\\unexpected' | Out-Null",
            ],
            text=True,
            encoding="utf-8",
            errors="replace",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        self.assertEqual(create_sibling.returncode, 0, msg=create_sibling.stderr)
        malformed_retry = self.invoke_raw(
            self.manifest(
                [self.command(env=self.fixture_env())], namespace=malformed_namespace
            ),
            fixture_opt_in=True,
        )
        self.assertEqual(malformed_retry.returncode, 1)
        self.assertIn("residual state", malformed_retry.stderr)

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_test_only_resource_and_writer_preflight_fails_closed(self) -> None:
        manifest = self.manifest([self.command(env=self.fixture_env())])
        manifest["windows_runtime"]["source_materialization"]["wsl_distro_name"] = (
            "FixtureDistro"
        )
        success, summary, result = self.invoke(manifest, fixture_opt_in=True)
        self.assertEqual(success.returncode, 0, msg=success.stderr)
        self.assertEqual(summary["status"], "success")
        evidence_dir = self.unix_path(str(summary["evidence_dir"]))
        preflight = json.loads(
            (evidence_dir / "preflight.json").read_text(encoding="utf-8")
        )
        self.assertTrue(
            all(
                check["source"] == "test-only-fixture"
                for check in preflight["execution_preflight"]
            )
        )
        self.assertEqual(
            [
                check["wsl_processes"]["wsl_distro_name"]
                for check in preflight["execution_preflight"]
            ],
            ["FixtureDistro", "FixtureDistro", "FixtureDistro"],
        )

        cases = (
            (
                "disk",
                {
                    **DEFAULT_PREFLIGHT_FIXTURE,
                    "CARGO_VALIDATE_WINDOWS_TEST_DISK_FREE_BYTES": str(119 * 1024**3),
                },
                "free bytes",
            ),
            (
                "memory",
                {
                    **DEFAULT_PREFLIGHT_FIXTURE,
                    "CARGO_VALIDATE_WINDOWS_TEST_AVAILABLE_MEMORY_BYTES": str(
                        29 * 1024**3
                    ),
                },
                "available Windows physical memory",
            ),
            (
                "native-writer",
                {
                    **DEFAULT_PREFLIGHT_FIXTURE,
                    "CARGO_VALIDATE_WINDOWS_TEST_NATIVE_PROCESS_STATE": "cargo",
                },
                "native Windows cargo writer",
            ),
            (
                "native-query-failure",
                {
                    **DEFAULT_PREFLIGHT_FIXTURE,
                    "CARGO_VALIDATE_WINDOWS_TEST_NATIVE_PROCESS_STATE": "query-failure",
                },
                "native Windows process query",
            ),
            (
                "native-malformed",
                {
                    **DEFAULT_PREFLIGHT_FIXTURE,
                    "CARGO_VALIDATE_WINDOWS_TEST_NATIVE_PROCESS_STATE": "malformed",
                },
                "malformed",
            ),
            (
                "wsl-writer",
                {
                    **DEFAULT_PREFLIGHT_FIXTURE,
                    "CARGO_VALIDATE_WINDOWS_TEST_WSL_PROCESS_STATE": "rustc",
                },
                "WSL cargo writer",
            ),
            (
                "wsl-query-failure",
                {
                    **DEFAULT_PREFLIGHT_FIXTURE,
                    "CARGO_VALIDATE_WINDOWS_TEST_WSL_PROCESS_STATE": "query-failure",
                },
                "wsl.exe process query",
            ),
            (
                "wsl-malformed",
                {
                    **DEFAULT_PREFLIGHT_FIXTURE,
                    "CARGO_VALIDATE_WINDOWS_TEST_WSL_PROCESS_STATE": "malformed",
                },
                "not parseable",
            ),
        )
        for label, fixture, expected in cases:
            with self.subTest(label=label):
                process, summary, result = self.invoke(
                    self.manifest([self.command(env=self.fixture_env())]),
                    fixture_opt_in=True,
                    preflight_fixture=fixture,
                )
                self.assertEqual(process.returncode, 1)
                self.assertEqual(summary["status"], "preflight-failed")
                self.assertIn(expected.casefold(), str(result["error"]).casefold())
                self.assertNotIn("OrderedDictionary", process.stderr)
                self.assertFalse(
                    self.unix_path(result["paths"]["tool_staging"])
                    .joinpath("command-1", "cargo.exe")
                    .exists(),
                    msg="preflight failure must occur before fake-cargo creation",
                )

        process = self.invoke_raw(
            self.manifest([self.command(env=self.fixture_env())]),
            fixture_opt_in=True,
            preflight_fixture=DEFAULT_PREFLIGHT_FIXTURE,
            preflight_fixture_opt_in=False,
        )
        self.assertEqual(process.returncode, 1)
        self.assertEqual(process.stdout.strip(), "")
        self.assertIn("preflight fixture data requires", process.stderr)

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_yolo_bypasses_only_resource_floors_and_records_it(self) -> None:
        yolo_manifest = self.manifest([self.command(env=self.fixture_env())])
        yolo_manifest["flags"].append("yolo")
        low_resources = {
            **DEFAULT_PREFLIGHT_FIXTURE,
            "CARGO_VALIDATE_WINDOWS_TEST_DISK_FREE_BYTES": str(119 * 1024**3),
            "CARGO_VALIDATE_WINDOWS_TEST_AVAILABLE_MEMORY_BYTES": str(29 * 1024**3),
        }
        process, summary, result = self.invoke(
            yolo_manifest,
            fixture_opt_in=True,
            preflight_fixture=low_resources,
        )
        self.assertEqual(process.returncode, 0, msg=process.stderr)
        self.assertEqual(summary["status"], "success")
        evidence_dir = self.unix_path(str(summary["evidence_dir"]))
        preflight = json.loads(
            (evidence_dir / "preflight.json").read_text(encoding="utf-8")
        )
        self.assertTrue(preflight["yolo"])
        self.assertTrue(
            all(
                check["yolo"]
                and check["bypassed_free_disk_floor"]
                and check["bypassed_available_memory_floor"]
                and check["free_disk_bytes"] == 119 * 1024**3
                and check["required_free_disk_bytes"] == 120 * 1024**3
                and check["available_memory_bytes"] == 29 * 1024**3
                and check["required_available_memory_bytes"] == 30 * 1024**3
                for check in preflight["execution_preflight"]
            )
        )
        command_result = next(
            record for record in result["command_results"] if record["index"] == 1
        )
        self.assertTrue(command_result["command_preflight"]["yolo"])
        self.assertTrue(command_result["command_preflight"]["bypassed_free_disk_floor"])
        self.assertTrue(
            command_result["command_preflight"]["bypassed_available_memory_floor"]
        )

        for label, fixture, expected in (
            (
                "native-writer",
                {
                    **DEFAULT_PREFLIGHT_FIXTURE,
                    "CARGO_VALIDATE_WINDOWS_TEST_NATIVE_PROCESS_STATE": "cargo",
                },
                "native Windows cargo writer",
            ),
            (
                "wsl-writer",
                {
                    **DEFAULT_PREFLIGHT_FIXTURE,
                    "CARGO_VALIDATE_WINDOWS_TEST_WSL_PROCESS_STATE": "rustc",
                },
                "WSL cargo writer",
            ),
        ):
            with self.subTest(label=label):
                manifest = self.manifest([self.command(env=self.fixture_env())])
                manifest["flags"].append("yolo")
                process, summary, result = self.invoke(
                    manifest,
                    fixture_opt_in=True,
                    preflight_fixture=fixture,
                )
                self.assertEqual(process.returncode, 1)
                self.assertEqual(summary["status"], "preflight-failed")
                self.assertIn(expected.casefold(), str(result["error"]).casefold())
                self.assertEqual([], result["command_results"])
                evidence_dir = self.unix_path(str(summary["evidence_dir"]))
                preflight = json.loads(
                    (evidence_dir / "preflight.json").read_text(encoding="utf-8")
                )
                self.assertTrue(preflight["yolo"])
                self.assertTrue(preflight["execution_preflight"][0]["yolo"])

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_runtime_disk_monitor_aborts_the_fake_process_tree_without_cleanup(
        self,
    ) -> None:
        source, identity, _ = self.create_source_fixture(
            include_symlink=False, include_msvc_setup=True
        )
        bootstrap_fixture, digests = self.make_bootstrap_fixture()
        samples = ",".join([str(128 * 1024**3)] * 8 + [str(4 * 1024**3)])
        fixture = {
            **DEFAULT_PREFLIGHT_FIXTURE,
            "CARGO_VALIDATE_WINDOWS_TEST_DISK_FREE_SAMPLES": samples,
        }
        manifest = self.manifest(
            [self.command(env=self.fixture_env(), artifact_policy="none")],
            candidate_identity=identity,
            source_root=source,
        )
        self.configure_bootstrap_runtime(manifest, digests)
        runtime = manifest["windows_runtime"]
        self.assertIsInstance(runtime, dict)
        namespace = runtime["workflow_namespace"]
        process, summary, result = self.invoke(
            manifest,
            fixture_opt_in=True,
            preflight_fixture=fixture,
            bootstrap_fixture=bootstrap_fixture,
            fake_long_running=True,
            fake_wait_for_readiness=True,
        )
        self.assertEqual(process.returncode, 1, msg=process.stderr)
        self.assertEqual(summary["status"], "command-failed")
        self.assertEqual(result["status"], "command-failed")
        command = self.command_result(process, result)
        self.assertEqual(command["status"], "resource-aborted")
        self.assertEqual(
            command["resource_abort"]["cause"], "native-runtime-disk-floor"
        )
        self.assertTrue(command["resource_abort"]["termination"]["terminated"])
        self.assertTrue(command["resource_abort"]["termination"]["quiescent"])
        self.assertEqual(
            command["resource_abort"]["sample"]["free_disk_bytes"], 4 * 1024**3
        )
        self.assertTrue(self.unix_path(result["paths"]["workset_root"]).is_dir())
        self.assertTrue(
            self.unix_path(result["paths"]["cache_state_path"]).is_file(),
            msg="a resource-aborted but structurally intact workset remains reusable",
        )
        child_pid_path = self.unix_path(str(command["fake_child_pid_path"]))
        self.assertTrue(
            child_pid_path.is_file(),
            msg="the monitored fake command must start its child",
        )
        child_pid = int(child_pid_path.read_text(encoding="utf-8").strip())
        self.assertFalse(self.native_process_is_alive(child_pid))

        warm_manifest = self.manifest(
            [self.command(env=self.fixture_env(), artifact_policy="none")],
            candidate_identity=identity,
            source_root=source,
            namespace=namespace,
        )
        self.configure_bootstrap_runtime(warm_manifest, digests)
        warm_process, warm_summary, warm_result = self.invoke(
            warm_manifest, fixture_opt_in=True
        )
        self.assertEqual(warm_process.returncode, 0, msg=warm_process.stderr)
        self.assertEqual(warm_summary["status"], "success")
        self.assertEqual(warm_result["bootstrap"]["source"], "reused")
        self.assertEqual(
            warm_result["paths"]["target_dir"], result["paths"]["target_dir"]
        )

        yolo_manifest = self.manifest([self.command(env=self.fixture_env())])
        yolo_manifest["flags"].append("yolo")
        low_fixture = {
            **fixture,
            "CARGO_VALIDATE_WINDOWS_TEST_DISK_FREE_BYTES": str(1 * 1024**3),
            "CARGO_VALIDATE_WINDOWS_TEST_AVAILABLE_MEMORY_BYTES": str(1 * 1024**3),
        }
        process, summary, result = self.invoke(
            yolo_manifest,
            fixture_opt_in=True,
            preflight_fixture=low_fixture,
            fake_long_running=True,
            fake_wait_for_readiness=True,
        )
        self.assertEqual(process.returncode, 1, msg=process.stderr)
        self.assertEqual(summary["status"], "command-failed")
        command = self.command_result(process, result)
        self.assertEqual(command["status"], "resource-aborted")
        self.assertTrue(command["command_preflight"]["yolo"])

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_root_exit_with_contained_descendant_uses_bounded_containment_cleanup(
        self,
    ) -> None:
        process, summary, result = self.invoke(
            self.manifest([self.command(env=self.fixture_env())]),
            fixture_opt_in=True,
            fake_root_exits_first=True,
            fake_wait_for_readiness=True,
        )

        self.assertEqual(process.returncode, 0, msg=process.stderr)
        self.assertEqual(summary["status"], "success")
        self.assertEqual(result["status"], "success")
        command = self.command_result(process, result)
        self.assertEqual(command["status"], "success")
        termination = command["termination"]
        self.assertEqual(
            termination["reason"], "root-exited-with-contained-descendants"
        )
        self.assertTrue(termination["root_exited_before_termination"])
        self.assertGreaterEqual(termination["active_processes_before_termination"], 1)
        self.assertTrue(termination["terminated"])
        self.assertTrue(termination["quiescent"])
        self.assertIsNone(termination["persistent_failure"])
        self.assertEqual(termination["active_processes_after_termination"], 0)
        self.assertEqual(command["containment"]["active_processes"], 0)
        child_pid_path = self.unix_path(str(command["fake_child_pid_path"]))
        self.assertTrue(child_pid_path.is_file())
        child_pid = int(child_pid_path.read_text(encoding="utf-8").strip())
        self.assertFalse(self.native_process_is_alive(child_pid))

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_root_exit_termination_failure_finishes_without_waiting_for_child_eof(
        self,
    ) -> None:
        started_at = time.monotonic()
        process, summary, result = self.invoke(
            self.manifest([self.command(env=self.fixture_env())]),
            fixture_opt_in=True,
            fake_root_exits_first=True,
            fake_immediate_descendant=True,
            fake_containment_terminate_failure=True,
            fake_wait_for_readiness=True,
        )

        self.assertLess(time.monotonic() - started_at, 20)
        self.assertEqual(process.returncode, 1, msg=process.stderr)
        self.assertEqual(summary["status"], "command-failed")
        self.assertEqual(result["status"], "command-failed")
        command = self.command_result(process, result)
        self.assertEqual(command["status"], "failed")
        self.assertEqual(command["exit_code"], 1)
        self.assertIsNone(command["resource_abort"])
        self.assertIsNone(command["monitor_error"])
        self.assertIn("could not terminate", command["post_launch_error"])
        termination = command["termination"]
        self.assertEqual(
            termination["reason"], "root-exited-with-contained-descendants"
        )
        self.assertTrue(termination["root_exited_before_termination"])
        self.assertGreaterEqual(termination["active_processes_before_termination"], 1)
        self.assertFalse(termination["terminated"])
        self.assertFalse(termination["quiescent"])
        self.assertIn(
            "test-only native command containment termination failure",
            termination["termination_error"],
        )
        self.assertIn("could not terminate", termination["persistent_failure"])
        self.assertIsNone(termination["active_processes_after_termination"])
        self.assertIsNone(command["containment"]["active_processes"])
        self.assertTrue(self.unix_path(str(command["stdout_path"])).is_file())
        self.assertTrue(self.unix_path(str(command["stderr_path"])).is_file())
        child_pid_path = self.unix_path(str(command["fake_child_pid_path"]))
        self.assertTrue(child_pid_path.is_file())
        child_pid = int(child_pid_path.read_text(encoding="utf-8").strip())
        self.assertFalse(self.native_process_is_alive(child_pid))

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_monitor_query_error_terminates_command_and_records_command_failure(
        self,
    ) -> None:
        fixture = {
            **DEFAULT_PREFLIGHT_FIXTURE,
            "CARGO_VALIDATE_WINDOWS_TEST_MONITOR_ERROR_AFTER_SAMPLES": "2",
        }
        process, summary, result = self.invoke(
            self.manifest([self.command(env=self.fixture_env())]),
            fixture_opt_in=True,
            preflight_fixture=fixture,
            fake_root_exits_first=True,
            fake_wait_for_readiness=True,
        )
        self.assertEqual(process.returncode, 1, msg=process.stderr)
        self.assertEqual(summary["status"], "command-failed")
        self.assertEqual(result["status"], "command-failed")
        command = self.command_result(process, result)
        self.assertEqual(command["status"], "failed")
        self.assertIn("monitor query failed", command["monitor_error"])
        self.assertIsNone(command["resource_abort"])
        self.assertEqual(command["termination"]["reason"], "monitor-query-error")
        self.assertTrue(command["termination"]["quiescent"])
        self.assertEqual(command["containment"]["active_processes"], 0)
        self.assertTrue(self.unix_path(str(command["stdout_path"])).is_file())
        self.assertTrue(self.unix_path(str(command["stderr_path"])).is_file())
        child_pid_path = self.unix_path(str(command["fake_child_pid_path"]))
        self.assertTrue(child_pid_path.is_file())
        child_pid = int(child_pid_path.read_text(encoding="utf-8").strip())
        self.assertFalse(self.native_process_is_alive(child_pid))

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_fake_readiness_timeout_terminates_the_contained_command(self) -> None:
        process, summary, result = self.invoke(
            self.manifest([self.command(env=self.fixture_env())]),
            fixture_opt_in=True,
            fake_long_running=True,
            fake_wait_for_readiness=True,
            fake_suppress_readiness=True,
        )

        self.assertEqual(process.returncode, 1, msg=process.stderr)
        self.assertEqual(summary["status"], "command-failed")
        self.assertEqual(result["status"], "command-failed")
        command = self.command_result(process, result)
        self.assertEqual(command["status"], "failed")
        self.assertEqual(
            command["monitor_error"],
            "test-only fake readiness timed out after 5000 milliseconds",
        )
        self.assertEqual(
            command["termination"]["reason"], "test-only-fake-readiness-timeout"
        )
        self.assertTrue(command["termination"]["quiescent"])
        self.assertEqual(command["containment"]["active_processes"], 0)
        self.assertTrue(self.unix_path(str(command["stdout_path"])).is_file())
        self.assertTrue(self.unix_path(str(command["stderr_path"])).is_file())
        self.assertTrue(self.unix_path(str(command["fake_argv_path"])).is_file())
        self.assertTrue(self.unix_path(str(command["fake_env_path"])).is_file())
        child_pid_path = self.unix_path(str(command["fake_child_pid_path"]))
        self.assertTrue(child_pid_path.is_file())
        child_pid = int(child_pid_path.read_text(encoding="utf-8").strip())
        self.assertFalse(self.native_process_is_alive(child_pid))

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_suspended_launch_enrolls_immediate_descendant_before_termination(
        self,
    ) -> None:
        fixture = {
            **DEFAULT_PREFLIGHT_FIXTURE,
            "CARGO_VALIDATE_WINDOWS_TEST_DISK_FREE_SAMPLES": (
                f"{128 * 1024**3},{128 * 1024**3},{128 * 1024**3},{4 * 1024**3}"
            ),
        }
        process, summary, result = self.invoke(
            self.manifest([self.command(env=self.fixture_env())]),
            fixture_opt_in=True,
            preflight_fixture=fixture,
            fake_immediate_descendant=True,
            fake_wait_for_readiness=True,
        )
        self.assertEqual(process.returncode, 1, msg=process.stderr)
        self.assertEqual(summary["status"], "command-failed")
        self.assertEqual(result["status"], "command-failed")
        command = self.command_result(process, result)
        self.assertEqual(command["status"], "resource-aborted")
        self.assertEqual(command["launch_mode"], "CreateProcessW CREATE_SUSPENDED")
        termination = command["resource_abort"]["termination"]
        self.assertGreaterEqual(termination["active_processes_before_termination"], 2)
        self.assertEqual(termination["active_processes_after_termination"], 0)
        self.assertTrue(termination["quiescent"])
        self.assertGreaterEqual(command["containment"]["peak_active_processes"], 2)
        self.assertTrue(self.unix_path(str(command["stdout_path"])).is_file())
        self.assertTrue(self.unix_path(str(command["stderr_path"])).is_file())
        child_pid_path = self.unix_path(str(command["fake_child_pid_path"]))
        self.assertTrue(child_pid_path.is_file())
        child_pid = int(child_pid_path.read_text(encoding="utf-8").strip())
        self.assertFalse(self.native_process_is_alive(child_pid))

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_native_mutex_busy_and_abandoned_fail_closed(self) -> None:
        manifest = self.manifest([self.command(env=self.fixture_env())])
        runtime = manifest["windows_runtime"]
        self.assertIsInstance(runtime, dict)
        namespace = runtime["workflow_namespace"]
        self.assertIsInstance(namespace, str)
        expected_run_root = self.unix_path(rf"F:\.cache\{namespace}")
        self.assertFalse(expected_run_root.exists())
        mutex_env = os.environ.copy()
        mutex_entries = [
            entry for entry in mutex_env.get("WSLENV", "").split(":") if entry
        ]
        mutex_entries = [
            entry
            for entry in mutex_entries
            if entry.split("/", 1)[0] != "CARGO_VALIDATE_WINDOWS_MUTEX_NAME"
        ]
        mutex_entries.append("CARGO_VALIDATE_WINDOWS_MUTEX_NAME")
        mutex_env["WSLENV"] = ":".join(mutex_entries)
        mutex_env["CARGO_VALIDATE_WINDOWS_MUTEX_NAME"] = NATIVE_MUTEX_NAME
        holder = subprocess.Popen(
            [
                PWSH,
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                (
                    "$m=[Threading.Mutex]::new($false,$env:CARGO_VALIDATE_WINDOWS_MUTEX_NAME); "
                    "if(-not $m.WaitOne(0)){exit 2}; "
                    'try{[Console]::Out.WriteLine("held");[Console]::Out.Flush();[void][Console]::In.ReadLine()} '
                    "finally{$m.ReleaseMutex();$m.Dispose()}"
                ),
            ],
            text=True,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=mutex_env,
        )
        try:
            self.assertEqual(holder.stdout.readline().strip(), "held")
            process = self.invoke_raw(manifest, fixture_opt_in=True)
            self.assertEqual(process.returncode, 1)
            self.assertEqual(process.stdout.strip(), "")
            self.assertIn("mutex is busy", process.stderr)
            self.assertFalse(expected_run_root.exists())
        finally:
            if holder.stdin is not None:
                holder.stdin.write("\n")
                holder.stdin.flush()
                holder.stdin.close()
            holder.wait(timeout=10)
            if holder.stdout is not None:
                holder.stdout.close()
            if holder.stderr is not None:
                holder.stderr.close()

        manifest = self.manifest([self.command(env=self.fixture_env())])
        runtime = manifest["windows_runtime"]
        self.assertIsInstance(runtime, dict)
        namespace = runtime["workflow_namespace"]
        self.assertIsInstance(namespace, str)
        expected_run_root = self.unix_path(rf"F:\.cache\{namespace}")
        self.assertFalse(expected_run_root.exists())
        process = self.invoke_raw(
            manifest,
            fixture_opt_in=True,
            preflight_fixture={
                **DEFAULT_PREFLIGHT_FIXTURE,
                "CARGO_VALIDATE_WINDOWS_TEST_MUTEX_STATE": "abandoned",
            },
        )
        self.assertEqual(process.returncode, 1)
        self.assertEqual(process.stdout.strip(), "")
        self.assertIn("mutex was abandoned", process.stderr)
        self.assertFalse(expected_run_root.exists())

    @unittest.skipUnless(
        Path(PWSH).is_file(),
        "PowerShell 7 is required for the Windows executor harness",
    )
    def test_candidate_accepts_ignored_untracked_file_after_fake_command(self) -> None:
        source, identity, before = self.create_source_fixture(include_symlink=False)
        process, summary, result = self.invoke(
            self.manifest(
                [self.command(env=self.fixture_env(), artifact_policy="none")],
                candidate_identity=identity,
                source_root=source,
            ),
            fixture_opt_in=True,
            fake_create_ignored=True,
        )
        self.assertEqual(process.returncode, 0, msg=process.stderr)
        self.assertEqual(summary["status"], "success")
        self.assertIsNone(result["error"])
        candidate = self.unix_path(result["paths"]["candidate_root"])
        self.assertTrue((candidate / "codex-rs" / ".fixture-ignored").is_file())
        self.assertIn(
            b".fixture-ignored\0",
            self.git_bytes(
                candidate,
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "-z",
            ),
        )
        self.assertTrue(
            self.unix_path(result["command_results"][0]["fake_argv_path"]).is_file()
        )
        self.assertGreater(
            result["candidate_materialization"]["candidate_after_command"][
                "ignored_untracked"
            ]["byte_length"],
            0,
        )
        self.assertEqual(
            result["candidate_materialization"]["source_after_command_loop"][
                "symbolic_head"
            ],
            before["symbolic_head"],
        )
        self.assertEqual(before, self.source_snapshot(source))


if __name__ == "__main__":
    if not Path(PWSH).is_file():
        print(f"PowerShell 7 executable is required: {PWSH}", file=sys.stderr)
        raise SystemExit(1)
    unittest.main()
