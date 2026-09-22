#!/usr/bin/env python3
"""Focused deterministic coverage for the Windows F: cache cleanup helper."""

import json
import ntpath
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import uuid
from pathlib import Path
from typing import Any

# Merge-safety anchor: all F: fixture writes go through PowerShell, remain below
# the dedicated test prefix, and actual native junction cleanup proves targets
# are not traversed while this harness refuses production deletion calls.

ROOT = Path(__file__).resolve().parents[1]
HELPER = ROOT / "scripts" / "clear-windows-build-cache.ps1"
PRODUCTION_TARGET = r"F:\.cache"
TEST_PREFIX = r"F:\.cache\cw\cleanup-tests"
OPT_IN = "COOLDEX_WINDOWS_CACHE_CLEANUP_TEST_ONLY"
SCRATCH = Path.home() / ".cache" / "codex" / "windows-cache-cleanup-tests"


def find_pwsh() -> str | None:
    return shutil.which("pwsh.exe") or shutil.which("pwsh")


PWSH = find_pwsh()


class HarnessPrerequisiteTests(unittest.TestCase):
    def test_direct_harness_fails_without_powershell(self) -> None:
        environment = os.environ.copy()
        environment["PATH"] = ""
        process = subprocess.run(
            [sys.executable, str(Path(__file__).resolve())],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            env=environment,
        )
        self.assertNotEqual(process.returncode, 0)
        self.assertIn("pwsh.exe or pwsh must be on PATH", process.stderr)


class CleanupTests(unittest.TestCase):
    def setUp(self) -> None:
        if PWSH is None:
            self.skipTest("pwsh.exe or pwsh must be on PATH")
        SCRATCH.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=SCRATCH, prefix="run.")
        self.work = Path(self.temp.name)
        self.roots: list[str] = []

    def tearDown(self) -> None:
        for root in reversed(self.roots):
            self.ps(
                "if (Test-Path -LiteralPath $env:CWC_PATH) { Remove-Item -LiteralPath $env:CWC_PATH -Recurse -Force }",
                {"CWC_PATH": root},
                check=False,
            )
        self.temp.cleanup()

    def windows_path(self, path: Path) -> str:
        result = subprocess.run(
            ["wslpath", "-w", str(path)], text=True, capture_output=True, check=False
        )
        self.assertEqual(
            result.returncode, 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
        )
        return result.stdout.strip()

    def environment(
        self, values: dict[str, str], opt_in: bool = True
    ) -> dict[str, str]:
        env = os.environ.copy()
        names = {
            item.split("/", 1)[0] for item in env.get("WSLENV", "").split(":") if item
        }
        env.update(values)
        names.update(values)
        if opt_in:
            env[OPT_IN] = "1"
            names.add(OPT_IN)
        else:
            env.pop(OPT_IN, None)
            names.discard(OPT_IN)
        env["WSLENV"] = ":".join(sorted(names))
        return env

    def ps(
        self, command: str, values: dict[str, str], *, check: bool = True
    ) -> subprocess.CompletedProcess[str]:
        assert PWSH is not None
        result = subprocess.run(
            [PWSH, "-NoLogo", "-NoProfile", "-NonInteractive", "-Command", command],
            text=True,
            capture_output=True,
            env=self.environment(values),
            check=False,
        )
        if check:
            self.assertEqual(result.returncode, 0, result.stderr)
        return result

    def synthetic_root(self, label: str, create: bool = True) -> str:
        root = rf"{TEST_PREFIX}\run-{uuid.uuid4().hex[:12]}-{label}"
        self.roots.append(root)
        if create:
            self.ps(
                "New-Item -ItemType Directory -Path $env:CWC_PATH -Force | Out-Null",
                {"CWC_PATH": root},
            )
        return root

    def write_file(self, root: str, relative: str, text: str = "fixture") -> str:
        self.ps(
            "$path = Join-Path $env:CWC_ROOT $env:CWC_REL; "
            "$parent = Split-Path -Parent $path; "
            "New-Item -ItemType Directory -Path $parent -Force | Out-Null; "
            "Set-Content -LiteralPath $path -Value $env:CWC_TEXT -NoNewline",
            {"CWC_ROOT": root, "CWC_REL": relative, "CWC_TEXT": text},
        )
        return ntpath.join(root, relative)

    def junction(self, link: str, target: str) -> None:
        self.ps(
            "New-Item -ItemType Junction -Path $env:CWC_LINK -Target $env:CWC_TARGET | Out-Null",
            {"CWC_LINK": link, "CWC_TARGET": target},
        )

    def exists(self, path: str) -> bool:
        result = self.ps(
            "[Console]::Out.Write((Test-Path -LiteralPath $env:CWC_PATH).ToString().ToLowerInvariant())",
            {"CWC_PATH": path},
        )
        return result.stdout == "true"

    def direct_count(self, root: str) -> int:
        result = self.ps(
            "[Console]::Out.Write(@(Get-ChildItem -LiteralPath $env:CWC_PATH -Force).Count)",
            {"CWC_PATH": root},
        )
        return int(result.stdout)

    def fixture(self, **changes: Any) -> Path:
        clean_windows = {
            "processes": [
                {
                    "ProcessId": 0,
                    "Name": "System Idle Process",
                    "ExecutablePath": None,
                    "CommandLine": None,
                }
            ]
        }
        clean_wsl = {"exit_code": 0, "stdout": "1 0 init /init\n"}
        data: dict[str, Any] = {
            "windows": [clean_windows, clean_windows],
            "wsl": [clean_wsl, clean_wsl],
        }
        data.update(changes)
        path = self.work / f"fixture-{uuid.uuid4().hex}.json"
        path.write_text(json.dumps(data), encoding="utf-8")
        return path

    def invoke(
        self, root: str, fixture: Path, *, delete: bool = False, opt_in: bool = True
    ) -> tuple[subprocess.CompletedProcess[str], dict[str, Any]]:
        if delete and root.casefold() == PRODUCTION_TARGET.casefold():
            raise AssertionError("the harness must never invoke production deletion")
        assert PWSH is not None
        args = [
            PWSH,
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            self.windows_path(HELPER),
            "-TestRoot",
            root,
            "-TestFixture",
            self.windows_path(fixture),
        ]
        if delete:
            args.append("-Delete")
        result = subprocess.run(
            args,
            text=True,
            capture_output=True,
            env=self.environment({}, opt_in),
            check=False,
        )
        lines = [line for line in result.stdout.splitlines() if line]
        self.assertEqual(
            len(lines), 1, f"stdout={result.stdout!r} stderr={result.stderr!r}"
        )
        return result, json.loads(lines[0])

    def assert_failed(
        self, root: str, fixture: Path, text: str, *, delete: bool = False
    ) -> None:
        result, summary = self.invoke(root, fixture, delete=delete)
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertEqual(summary["status"], "failed")
        self.assertIn(text.casefold(), summary["error"].casefold())

    def test_parser_preflight_and_fixed_production_authority(self) -> None:
        source = HELPER.read_text(encoding="utf-8")
        self.assertIn("Merge-safety anchor:", source)
        self.assertIn('ProductionTarget = "F:\\.cache"', source)
        self.assertIn("Set-StrictMode -Version Latest", source)
        self.assertIn('$ErrorActionPreference = "Stop"', source)
        self.assertIn('$ProgressPreference = "SilentlyContinue"', source)
        self.assertIn("ProcessStartInfo", source)
        self.assertIn("ArgumentList.Add", source)
        self.assertNotIn("[string]$Target", source.split("Set-StrictMode", 1)[0])
        parsed = self.ps(
            "$tokens=$null; $errors=$null; [void][System.Management.Automation.Language.Parser]::ParseFile($env:CWC_HELPER, [ref]$tokens, [ref]$errors); exit [int]($errors.Count -ne 0)",
            {"CWC_HELPER": self.windows_path(HELPER)},
        )
        self.assertEqual(parsed.returncode, 0, parsed.stderr)
        root = self.synthetic_root("preflight")
        child = self.write_file(root, "keep.txt")
        result, summary = self.invoke(root, self.fixture())
        self.assertEqual(
            result.returncode, 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
        )
        self.assertEqual(summary["mode"], "preflight")
        self.assertEqual(summary["status"], "preflight-ok")
        self.assertEqual(summary["target"].casefold(), root.casefold())
        self.assertEqual(summary["deleted_count"], 0)
        self.assertEqual(summary["remaining_count"], 1)
        self.assertEqual(summary["windows_scan_status"], "clean")
        self.assertEqual(summary["wsl_scan_status"], "clean")
        self.assertTrue(self.exists(child))
        with self.assertRaises(AssertionError):
            self.invoke(PRODUCTION_TARGET, self.fixture(), delete=True)

    def test_test_override_and_invalid_roots_fail_closed(self) -> None:
        root = self.synthetic_root("optin")
        child = self.write_file(root, "keep.txt")
        result, summary = self.invoke(root, self.fixture(), delete=True, opt_in=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(OPT_IN, summary["error"])
        self.assertTrue(self.exists(child))
        self.assert_failed(
            rf"F:\.cache\cw\not-admitted-{uuid.uuid4().hex[:6]}",
            self.fixture(),
            "TestRoot",
        )
        self.assert_failed(
            rf"{TEST_PREFIX}\run-{uuid.uuid4().hex[:6]}\..\other",
            self.fixture(),
            "normalized",
        )
        missing = self.synthetic_root("missing", create=False)
        self.assert_failed(missing, self.fixture(), "missing")
        file_root = self.synthetic_root("file", create=False)
        self.write_file(TEST_PREFIX, ntpath.basename(file_root), "file")
        self.assert_failed(file_root, self.fixture(), "directories")

    def test_multiline_wsl_ps_fixture_scans_cleanly(self) -> None:
        root = self.synthetic_root("multiline-wsl")
        child = self.write_file(root, "keep.txt")
        fixture = self.fixture(
            wsl=[
                {"exit_code": 0, "stdout": "1 0 init /init\n42 1 bash bash -lc true\n"}
            ]
        )
        result, summary = self.invoke(root, fixture)
        self.assertEqual(
            result.returncode, 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
        )
        self.assertEqual(summary["status"], "preflight-ok")
        self.assertEqual(summary["wsl_scan_status"], "clean")
        self.assertTrue(self.exists(child))

    def test_junction_cleanup_preserves_external_target_and_rejects_root_junction(
        self,
    ) -> None:
        fixture = self.fixture()
        target = self.synthetic_root("root-link-target")
        root_link = self.synthetic_root("root-link", create=False)
        self.junction(root_link, target)
        self.assert_failed(root_link, fixture, "reparse", delete=True)
        self.assertTrue(self.exists(target))

        root = self.synthetic_root("junction-cleanup")
        in_tree_sentinel = self.write_file(root, r"z-in-tree\sentinel.txt")
        internal_junction = ntpath.join(root, "a-internal-junction")
        self.junction(internal_junction, ntpath.join(root, "z-in-tree"))
        outside = self.synthetic_root("outside")
        outside_sentinel = self.write_file(outside, "sentinel.txt")
        external_junction = ntpath.join(root, "b-external-junction")
        self.junction(external_junction, outside)
        nested_in_tree_sentinel = self.write_file(
            root, r"m-recursive\z-in-tree\sentinel.txt"
        )
        nested_internal_junction = ntpath.join(
            root, "m-recursive", "a-internal-junction"
        )
        self.junction(
            nested_internal_junction, ntpath.join(root, "m-recursive", "z-in-tree")
        )
        nested_outside = self.synthetic_root("nested-outside")
        nested_outside_sentinel = self.write_file(nested_outside, "sentinel.txt")
        nested_external_junction = ntpath.join(
            root, "m-recursive", "b-external-junction"
        )
        self.junction(nested_external_junction, nested_outside)

        result, summary = self.invoke(root, fixture, delete=True)
        self.assertEqual(
            result.returncode, 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
        )
        self.assertEqual(summary["status"], "deleted")
        self.assertEqual(summary["deleted_count"], 4)
        self.assertEqual(summary["remaining_count"], 0)
        self.assertTrue(self.exists(root))
        self.assertEqual(self.direct_count(root), 0)
        self.assertFalse(self.exists(internal_junction))
        self.assertFalse(self.exists(external_junction))
        self.assertFalse(self.exists(in_tree_sentinel))
        self.assertTrue(self.exists(outside_sentinel))
        self.assertFalse(self.exists(nested_internal_junction))
        self.assertFalse(self.exists(nested_external_junction))
        self.assertFalse(self.exists(nested_in_tree_sentinel))
        self.assertTrue(self.exists(nested_outside_sentinel))

    def test_remove_child_parent_mismatch_stops_before_deletion(self) -> None:
        root = self.synthetic_root("parent-mismatch")
        keep = self.write_file(root, "keep.txt")
        outside = self.synthetic_root("parent-mismatch-outside")
        captured = self.write_file(outside, "outside.txt")
        result = self.ps(
            """
$source = Get-Content -LiteralPath $env:CWC_HELPER -Raw
$markerIndex = $source.IndexOf('$context = $null', [System.StringComparison]::Ordinal)
if ($markerIndex -lt 0) { exit 10 }
. ([ScriptBlock]::Create($source.Substring(0, $markerIndex)))
$item = Get-Item -LiteralPath $env:CWC_PATH -Force -ErrorAction Stop
if (Is-Reparse $item) { exit 11 }
try {
    Remove-Child ([pscustomobject]@{ test = $false; fixture = $null }) $env:CWC_ROOT $env:CWC_PATH
    exit 12
} catch {
    if ($_.Exception.Message -ne "captured child is no longer a direct supported child") {
        exit 13
    }
}
if (-not (Test-Path -LiteralPath $env:CWC_PATH)) { exit 14 }
[Console]::Out.Write("blocked")
""",
            {
                "CWC_HELPER": self.windows_path(HELPER),
                "CWC_ROOT": root,
                "CWC_PATH": captured,
            },
        )
        self.assertEqual(result.stdout, "blocked")
        self.assertTrue(self.exists(keep))
        self.assertTrue(self.exists(captured))
        self.assertEqual(self.direct_count(root), 1)
        self.assertEqual(self.direct_count(outside), 1)

    def test_writer_scan_errors_and_malformed_data_block(self) -> None:
        cases = [
            (
                "windows cargo",
                {
                    "windows": [
                        {
                            "processes": [
                                {
                                    "ProcessId": 9,
                                    "Name": "cargo.exe",
                                    "ExecutablePath": None,
                                    "CommandLine": None,
                                }
                            ]
                        }
                    ]
                },
                "writer",
            ),
            (
                "windows target",
                {
                    "windows": [
                        {
                            "processes": [
                                {
                                    "ProcessId": 9,
                                    "Name": "git.exe",
                                    "ExecutablePath": None,
                                    "CommandLine": r"git status F:\.cache",
                                }
                            ]
                        }
                    ]
                },
                "writer",
            ),
            (
                "wsl cargo",
                {
                    "wsl": [
                        {
                            "exit_code": 0,
                            "stdout": "1 0 init /init\n9 1 cargo cargo test\n",
                        }
                    ]
                },
                "writer",
            ),
            (
                "wsl target",
                {
                    "wsl": [
                        {"exit_code": 0, "stdout": "9 1 bash bash /mnt/f/.cache/x\n"}
                    ]
                },
                "writer",
            ),
            ("cim error", {"windows": [{"error": "denied"}]}, "CIM"),
            ("wsl error", {"wsl": [{"error": "down"}]}, "wsl.exe"),
            (
                "wsl malformed",
                {"wsl": [{"exit_code": 0, "stdout": "not-ps-output"}]},
                "parseable",
            ),
            ("malformed", {"windows": [{"processes": "not-an-array"}]}, "array"),
        ]
        for label, changes, expected in cases:
            with self.subTest(label=label):
                root = self.synthetic_root(label.replace(" ", "-"))
                child = self.write_file(root, "keep.txt")
                result, summary = self.invoke(root, self.fixture(**changes))
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected.casefold(), summary["error"].casefold())
                self.assertTrue(self.exists(child))

    def test_actual_mutex_and_second_snapshot_blocks_before_delete(self) -> None:
        root = self.synthetic_root("mutex")
        child = self.write_file(root, "keep.txt")
        holder = subprocess.Popen(
            [
                PWSH,
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                '$m=[Threading.Mutex]::new($false,"Local\\Cooldex.WindowsBuildCacheCleanup.v1"); if(-not $m.WaitOne(0)){exit 2}; try{[Console]::Out.WriteLine("held");[Console]::Out.Flush();[void][Console]::In.ReadLine()} finally{$m.ReleaseMutex();$m.Dispose()}',
            ],
            text=True,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=self.environment({}),
        )
        try:
            assert holder.stdout is not None
            self.assertEqual(holder.stdout.readline().strip(), "held")
            result, summary = self.invoke(root, self.fixture(), delete=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("mutex", summary["error"].casefold())
            self.assertEqual(summary["deleted_count"], 0)
            self.assertTrue(self.exists(child))
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
        for changes, expected in [
            ({"add_before_second": "added.txt"}, "tree changed"),
            (
                {
                    "windows": [
                        self.fixture_data_windows(),
                        {
                            "processes": [
                                {
                                    "ProcessId": 9,
                                    "Name": "rustc.exe",
                                    "ExecutablePath": None,
                                    "CommandLine": None,
                                }
                            ]
                        },
                    ]
                },
                "writer",
            ),
        ]:
            root = self.synthetic_root("second")
            keep = self.write_file(root, "keep.txt")
            result, summary = self.invoke(root, self.fixture(**changes), delete=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(expected, summary["error"].casefold())
            self.assertEqual(summary["deleted_count"], 0)
            self.assertTrue(self.exists(keep))

    @staticmethod
    def fixture_data_windows() -> dict[str, Any]:
        return {
            "processes": [
                {
                    "ProcessId": 0,
                    "Name": "System Idle Process",
                    "ExecutablePath": None,
                    "CommandLine": None,
                }
            ]
        }

    def test_delete_only_direct_children_success_and_partial_failure(self) -> None:
        root = self.synthetic_root("success")
        self.write_file(root, "file.txt")
        self.write_file(root, r"folder\nested.txt")
        result, summary = self.invoke(root, self.fixture(), delete=True)
        self.assertEqual(
            result.returncode, 0, f"stdout={result.stdout!r} stderr={result.stderr!r}"
        )
        self.assertEqual(summary["status"], "deleted")
        self.assertEqual(summary["deleted_count"], 2)
        self.assertEqual(summary["remaining_count"], 0)
        self.assertTrue(self.exists(root))
        self.assertEqual(self.direct_count(root), 0)
        partial = self.synthetic_root("partial")
        first = self.write_file(partial, "a.txt")
        blocked = self.write_file(partial, "b.txt")
        result, summary = self.invoke(
            partial, self.fixture(fail_on="b.txt"), delete=True
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(summary["deleted_count"], 1)
        self.assertEqual(summary["remaining_count"], 1)
        self.assertFalse(self.exists(first))
        self.assertTrue(self.exists(blocked))


if __name__ == "__main__":
    if PWSH is None:
        print("pwsh.exe or pwsh must be on PATH", file=sys.stderr)
        raise SystemExit(1)
    unittest.main(verbosity=2)
