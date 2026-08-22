#!/usr/bin/env python3
"""Unit tests for the deterministic Cargo validation planner."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import queue
import signal
import subprocess
import sys
import tempfile
import threading
import textwrap
import time
import unittest
from pathlib import Path
from unittest import mock

# Merge-safety anchor: these tests protect cargo-validate planner selection,
# verify execution semantics, and receipt contracts without compiling Rust.

REPO_ROOT = Path(__file__).resolve().parents[1]
PLANNER = REPO_ROOT / "scripts" / "cargo-validate.py"
PRODUCTION_CONFIG = REPO_ROOT / "scripts" / "cargo-validation.toml"


def load_planner_module() -> object:
    spec = importlib.util.spec_from_file_location("cargo_validate_under_test", PLANNER)
    if spec is None or spec.loader is None:
        raise AssertionError(f"failed to load planner module from {PLANNER}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class CargoValidateTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory(prefix="cargo-validate-tests.")
        self.repo_root = Path(self.temp_dir.name)
        package_roots = {
            "codex-analytics": "analytics",
            "codex-app-server": "app-server",
            "codex-app-server-protocol": "app-server-protocol",
            "codex-app-server-transport": "app-server-transport",
            "codex-backend-client": "backend-client",
            "codex-chatgpt": "chatgpt",
            "codex-cli": "cli",
            "codex-cloud-config": "cloud-config",
            "codex-cloud-tasks": "cloud-tasks",
            "codex-config": "config",
            "codex-core": "core",
            "codex-core-api": "core-api",
            "codex-code-mode-host": "code-mode-host",
            "codex-code-mode-protocol": "code-mode-protocol",
            "codex-backend-openapi-models": "codex-backend-openapi-models",
            "codex-exec": "exec",
            "codex-exec-server-protocol": "exec-server-protocol",
            "codex-extension-items": "ext/items",
            "codex-features": "features",
            "codex-feedback": "feedback",
            "codex-file-system": "file-system",
            "codex-git-utils": "git-utils",
            "codex-hooks": "hooks",
            "codex-http-client": "http-client",
            "codex-install-context": "install-context",
            "codex-linux-sandbox": "linux-sandbox",
            "codex-login": "login",
            "codex-mcp": "codex-mcp",
            "codex-model-provider": "model-provider",
            "codex-models-manager": "models-manager",
            "codex-protocol": "protocol",
            "codex-rmcp-client": "rmcp-client",
            "codex-sandboxing": "sandboxing",
            "codex-test-binary-support": "test-binary-support",
            "codex-thread-manager-sample": "thread-manager-sample",
            "codex-tools": "tools",
            "codex-tui": "tui",
            "codex-unmapped-fixture": "unmapped-fixture",
            "codex-utils-process": "utils/process",
            "codex-websocket-client": "websocket-client",
        }
        packages = []
        for package_name, package_root in package_roots.items():
            manifest_path = self.repo_root / "codex-rs" / package_root / "Cargo.toml"
            manifest_path.parent.mkdir(parents=True, exist_ok=True)
            manifest_path.write_text(f'[package]\nname = "{package_name}"\n')
            has_runnable_targets = package_name != "codex-core-api"
            packages.append(
                {
                    "name": package_name,
                    "manifest_path": str(manifest_path),
                    "targets": [
                        {
                            "test": has_runnable_targets,
                            "doctest": has_runnable_targets,
                        }
                    ],
                }
            )

        (self.repo_root / "codex-rs" / "core" / "src" / "config").mkdir(parents=True)
        (self.repo_root / "codex-rs" / "core" / "src" / "config" / "mod.rs").write_text(
            "pub fn config() {}\n"
        )
        (self.repo_root / "codex-rs" / "core" / "src" / "feature.rs").write_text(
            '#[cfg(feature = "danger")]\npub fn feature_gate() {}\n'
        )
        (self.repo_root / "codex-rs" / "core" / "src" / "feature_all.rs").write_text(
            '#[cfg(all(unix, feature = "danger"))]\npub fn feature_gate() {}\n'
        )
        (self.repo_root / "codex-rs" / "core" / "src" / "feature_any.rs").write_text(
            '#[cfg(any(feature = "a", feature = "b"))]\npub fn feature_gate() {}\n'
        )
        (self.repo_root / "codex-rs" / "core" / "src" / "feature_attr.rs").write_text(
            '#[cfg_attr(feature = "danger", derive(Debug))]\npub struct FeatureGate;\n'
        )
        (self.repo_root / "codex-rs" / "core" / "src" / "feature_macro.rs").write_text(
            'pub fn feature_gate() -> bool { cfg!(feature = "danger") }\n'
        )
        (self.repo_root / "codex-rs" / "tui" / "src").mkdir(parents=True)
        (self.repo_root / "codex-rs" / "tui" / "src" / "app.rs").write_text(
            "pub fn app() {}\n"
        )
        (self.repo_root / "codex-rs" / "models-manager" / "src").mkdir(parents=True)
        (
            self.repo_root / "codex-rs" / "models-manager" / "src" / "manager.rs"
        ).write_text("pub fn manager() {}\n")
        (self.repo_root / "codex-rs" / "unmapped-fixture" / "src").mkdir(parents=True)
        (
            self.repo_root / "codex-rs" / "unmapped-fixture" / "src" / "lib.rs"
        ).write_text("pub fn unmapped_fixture() {}\n")
        (self.repo_root / "codex-rs" / "features" / "src").mkdir(parents=True)
        (self.repo_root / "codex-rs" / "features" / "src" / "lib.rs").write_text(
            "pub fn features() {}\n"
        )
        (self.repo_root / "codex-rs" / "protocol" / "src").mkdir(parents=True)
        (
            self.repo_root / "codex-rs" / "protocol" / "src" / "permissions.rs"
        ).write_text("pub fn permissions() {}\n")
        (self.repo_root / "codex-rs" / "app-server-transport" / "src").mkdir(
            parents=True
        )
        (
            self.repo_root / "codex-rs" / "app-server-transport" / "src" / "lib.rs"
        ).write_text("pub fn transport() {}\n")
        (self.repo_root / "codex-rs" / "cloud-config" / "src").mkdir(parents=True)
        (self.repo_root / "codex-rs" / "cloud-config" / "src" / "lib.rs").write_text(
            "pub fn cloud_config() {}\n"
        )
        (self.repo_root / "codex-rs" / "tools" / "src").mkdir(parents=True)
        (self.repo_root / "codex-rs" / "tools" / "src" / "tool_config.rs").write_text(
            "pub fn tool_config() {}\n"
        )
        (self.repo_root / "codex-rs" / "analytics" / "src").mkdir(parents=True)
        (self.repo_root / "codex-rs" / "analytics" / "src" / "reducer.rs").write_text(
            "pub fn reducer() {}\n"
        )
        (self.repo_root / "codex-rs" / "codex-mcp" / "src" / "mcp").mkdir(parents=True)
        (
            self.repo_root / "codex-rs" / "codex-mcp" / "src" / "mcp" / "mod.rs"
        ).write_text("pub fn mcp() {}\n")
        (self.repo_root / "codex-rs" / "file-system" / "src").mkdir(parents=True)
        (self.repo_root / "codex-rs" / "file-system" / "src" / "lib.rs").write_text(
            "pub fn file_system() {}\n"
        )
        (self.repo_root / "codex-rs" / "thread-manager-sample" / "src").mkdir(
            parents=True
        )
        (
            self.repo_root / "codex-rs" / "thread-manager-sample" / "src" / "main.rs"
        ).write_text("fn main() {}\n")
        (self.repo_root / "codex-rs" / "test-binary-support" / "lib.rs").write_text(
            "pub fn configure() {}\n"
        )
        self.metadata_path = self.repo_root / "metadata.json"
        self.metadata_path.write_text(json.dumps({"packages": packages}))

    def tearDown(self) -> None:
        self.temp_dir.cleanup()

    def run_planner(
        self, *args: str, check: bool = True, env: dict[str, str] | None = None
    ) -> subprocess.CompletedProcess[str]:
        process = subprocess.run(
            [sys.executable, str(PLANNER), *args],
            cwd=REPO_ROOT,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if check and process.returncode != 0:
            self.fail(
                f"planner failed: {process.returncode}\nSTDOUT:\n{process.stdout}\nSTDERR:\n{process.stderr}"
            )
        return process

    def plan_json(
        self, *args: str, receipt_dir: Path | None = None
    ) -> dict[str, object]:
        return self.action_json("plan", *args, receipt_dir=receipt_dir)

    def action_json(
        self, action: str, *args: str, receipt_dir: Path | None = None
    ) -> dict[str, object]:
        receipt_args = (
            ["--no-receipt"]
            if receipt_dir is None
            else ["--receipt-dir", str(receipt_dir)]
        )
        process = self.run_planner(
            action,
            "--json",
            *receipt_args,
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
            *args,
        )
        return json.loads(process.stdout)

    def command_lines(self, plan: dict[str, object]) -> list[list[str]]:
        return [command["argv"] for command in plan["commands"]]  # type: ignore[index]

    def command_for_argv(
        self, plan: dict[str, object], argv: list[str]
    ) -> dict[str, object]:
        for command in plan["commands"]:  # type: ignore[index]
            if command["argv"] == argv:
                return command
        self.fail(f"missing command argv: {argv}")

    def test_plan_help_works_without_codex_repo_root_env(self) -> None:
        env = os.environ.copy()
        env.pop("CODEX_REPO_ROOT", None)

        process = subprocess.run(
            [sys.executable, str(PLANNER), "plan", "--help"],
            cwd=REPO_ROOT,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )

        self.assertEqual(
            process.returncode,
            0,
            msg=(
                f"plan --help should succeed without CODEX_REPO_ROOT\n"
                f"STDOUT:\n{process.stdout}\nSTDERR:\n{process.stderr}"
            ),
        )
        self.assertIn("usage:", process.stdout)

    def init_git_repo(self) -> None:
        subprocess.run(
            ["git", "init"],
            cwd=self.repo_root,
            stdout=subprocess.DEVNULL,
            check=True,
        )
        subprocess.run(
            ["git", "checkout", "-B", "main"],
            cwd=self.repo_root,
            stdout=subprocess.DEVNULL,
            check=True,
        )
        subprocess.run(
            ["git", "config", "user.name", "Test"],
            cwd=self.repo_root,
            check=True,
        )
        subprocess.run(
            ["git", "config", "user.email", "test@example.com"],
            cwd=self.repo_root,
            check=True,
        )

    def commit_all(self, message: str) -> str:
        subprocess.run(["git", "add", "-A"], cwd=self.repo_root, check=True)
        subprocess.run(
            ["git", "commit", "-m", message],
            cwd=self.repo_root,
            stdout=subprocess.DEVNULL,
            check=True,
        )
        process = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=self.repo_root,
            text=True,
            stdout=subprocess.PIPE,
            check=True,
        )
        return process.stdout.strip()

    def test_commit_selector_uses_non_merge_commit_changed_files(self) -> None:
        self.init_git_repo()
        self.commit_all("initial")
        config_path = self.repo_root / "codex-rs" / "core" / "src" / "config" / "mod.rs"
        config_path.write_text("pub fn config() { let value = 1; }\n")
        self.commit_all("update core config")

        plan = self.plan_json("--commit", "HEAD", "--mode", "standard")

        self.assertEqual(["codex-rs/core/src/config/mod.rs"], plan["changed_files"])
        self.assertIn("codex-core", plan["selected_packages"])
        self.assertIn("cli", plan["selected_surfaces"])

    def test_range_selector_includes_deleted_paths_and_dedupes_files(self) -> None:
        self.init_git_repo()
        base_commit = self.commit_all("initial")
        config_path = self.repo_root / "codex-rs" / "core" / "src" / "config" / "mod.rs"
        config_path.write_text("pub fn config() { let value = 2; }\n")
        deleted_path = self.repo_root / "codex-rs" / "tools" / "src" / "tool_config.rs"
        deleted_path.unlink()
        self.commit_all("update range files")

        plan = self.plan_json(
            "--range",
            f"{base_commit}..HEAD",
            "--file",
            "codex-rs/core/src/config/mod.rs",
            "--mode",
            "standard",
        )

        changed_files = list(plan["changed_files"])  # type: ignore[arg-type]
        self.assertEqual(1, changed_files.count("codex-rs/core/src/config/mod.rs"))
        self.assertIn("codex-rs/tools/src/tool_config.rs", changed_files)
        self.assertIn("codex-core", plan["selected_packages"])
        self.assertIn("codex-tools", plan["selected_packages"])

    def test_range_selector_ignores_non_feature_cfg_and_feature_field(self) -> None:
        self.init_git_repo()
        config_manager_path = (
            self.repo_root / "codex-rs" / "app-server" / "src" / "config_manager.rs"
        )
        config_manager_path.parent.mkdir(parents=True, exist_ok=True)
        config_manager_path.write_text("pub fn config_name() {}\n")
        base_commit = self.commit_all("initial")
        config_manager_path.write_text(
            textwrap.dedent(
                """\
                #[cfg(test)]
                mod tests {
                    #[test]
                    fn records_config_name() {}
                }

                pub fn config_name(name: &str) {
                    tracing::debug!(feature = name);
                }
                """
            )
        )
        self.commit_all("update config manager")

        plan = self.plan_json(
            "--range",
            f"{base_commit}..HEAD",
            "--mode",
            "standard",
        )

        self.assertEqual(
            ["codex-rs/app-server/src/config_manager.rs"], plan["changed_files"]
        )
        self.assertIn("codex-app-server", plan["selected_packages"])

    def test_commit_selector_rejects_merge_commits(self) -> None:
        self.init_git_repo()
        self.commit_all("initial")
        subprocess.run(
            ["git", "checkout", "-b", "side"], cwd=self.repo_root, check=True
        )
        tui_path = self.repo_root / "codex-rs" / "tui" / "src" / "app.rs"
        tui_path.write_text("pub fn app() { let value = 1; }\n")
        self.commit_all("side update")
        subprocess.run(["git", "checkout", "main"], cwd=self.repo_root, check=True)
        config_path = self.repo_root / "codex-rs" / "core" / "src" / "config" / "mod.rs"
        config_path.write_text("pub fn config() { let value = 3; }\n")
        self.commit_all("main update")
        subprocess.run(
            ["git", "merge", "--no-ff", "side", "-m", "merge side"],
            cwd=self.repo_root,
            stdout=subprocess.DEVNULL,
            check=True,
        )

        process = self.run_planner(
            "plan",
            "--commit",
            "HEAD",
            "--mode",
            "standard",
            "--no-receipt",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
            check=False,
        )

        self.assertNotEqual(0, process.returncode)
        self.assertIn("resolves to a merge commit", process.stderr)
        self.assertIn("--range <base>..HEAD", process.stderr)

    def test_invalid_range_selector_fails_loud(self) -> None:
        self.init_git_repo()
        self.commit_all("initial")

        process = self.run_planner(
            "plan",
            "--range",
            "missing-base..HEAD",
            "--mode",
            "standard",
            "--no-receipt",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
            check=False,
        )

        self.assertNotEqual(0, process.returncode)
        self.assertIn("command failed", process.stderr)
        self.assertIn("git diff --name-status missing-base..HEAD", process.stderr)

    def test_revision_selectors_accept_committed_deleted_package(self) -> None:
        deleted_manifest = (
            self.repo_root / "codex-rs" / "deleted-package" / "Cargo.toml"
        )
        deleted_source = (
            self.repo_root / "codex-rs" / "deleted-package" / "src" / "lib.rs"
        )
        deleted_source.parent.mkdir(parents=True)
        deleted_manifest.write_text('[package]\nname = "codex-deleted-package"\n')
        deleted_source.write_text("pub fn deleted_package() {}\n")
        self.init_git_repo()
        base_commit = self.commit_all("add deleted package")
        deleted_source.unlink()
        deleted_manifest.unlink()
        deletion_commit = self.commit_all("delete package")
        expected_files = [
            "codex-rs/deleted-package/Cargo.toml",
            "codex-rs/deleted-package/src/lib.rs",
        ]
        selector_cases = {
            "range": ["--range", f"{base_commit}..{deletion_commit}"],
            "commit": ["--commit", deletion_commit],
            "overlapping_range_and_commit": [
                "--range",
                f"{base_commit}..{deletion_commit}",
                "--commit",
                deletion_commit,
            ],
        }

        for case_name, selectors in selector_cases.items():
            with self.subTest(case_name):
                plan = self.action_json("prep-plan", *selectors, "--mode", "standard")
                commands = self.command_lines(plan)

                self.assertEqual(expected_files, plan["changed_files"])
                self.assertEqual(
                    1,
                    list(plan["changed_files"]).count(expected_files[0]),
                )
                self.assertEqual(
                    1,
                    list(plan["changed_files"]).count(expected_files[1]),
                )
                self.assertIn("rust_source", plan["flags"])
                self.assertIn("manifest_changed", plan["flags"])
                self.assertIn(["just", "fmt"], commands)
                self.assertIn(["just", "bazel-lock-update"], commands)
                self.assertNotIn("codex-deleted-package", plan["selected_packages"])

    def test_deleted_revision_provenance_does_not_override_readd_or_explicit_file(
        self,
    ) -> None:
        readded_path = (
            self.repo_root / "codex-rs" / "readded-package" / "src" / "lib.rs"
        )
        explicit_path = (
            self.repo_root / "codex-rs" / "explicit-package" / "src" / "lib.rs"
        )
        readded_path.parent.mkdir(parents=True)
        explicit_path.parent.mkdir(parents=True)
        readded_path.write_text("pub fn original_readded() {}\n")
        explicit_path.write_text("pub fn original_explicit() {}\n")
        self.init_git_repo()
        base_commit = self.commit_all("add unowned paths")
        readded_path.unlink()
        explicit_path.unlink()
        deletion_commit = self.commit_all("delete unowned paths")
        readded_path.write_text("pub fn readded() {}\n")
        readd_commit = self.commit_all("readd unowned path")

        selector_cases = {
            "revision_readd": [
                "--range",
                f"{base_commit}..{deletion_commit}",
                "--commit",
                readd_commit,
            ],
            "explicit_absent_path": [
                "--range",
                f"{base_commit}..{deletion_commit}",
                "--file",
                "codex-rs/explicit-package/src/lib.rs",
            ],
        }

        for case_name, selectors in selector_cases.items():
            with self.subTest(case_name):
                process = self.run_planner(
                    "prep-plan",
                    *selectors,
                    "--mode",
                    "standard",
                    "--no-receipt",
                    "--repo-root",
                    str(self.repo_root),
                    "--metadata-json",
                    str(self.metadata_path),
                    "--config",
                    str(PRODUCTION_CONFIG),
                    check=False,
                )

                self.assertNotEqual(0, process.returncode)
                self.assertIn("not owned by a Cargo workspace package", process.stderr)

    def test_revision_name_status_parser_handles_scored_renames_copies_and_malformed_records(
        self,
    ) -> None:
        planner = load_planner_module()

        selections = planner.parse_revision_name_status(
            "R082\told.rs\tnew.rs\n"
            "C100\tsource.rs\tcopy.rs\n"
            "D\tdeleted.rs\n"
            "M\tmodified.rs\n",
            self.repo_root,
        )

        self.assertEqual(
            [
                ("new.rs", False),
                ("copy.rs", False),
                ("deleted.rs", True),
                ("modified.rs", False),
            ],
            [(selection.path, selection.deleted) for selection in selections],
        )
        malformed_outputs = [
            "X\tunknown.rs\n",
            "R100\told.rs\n",
            "Rabc\told.rs\tnew.rs\n",
            "M\tmodified.rs\textra.rs\n",
        ]
        for output in malformed_outputs:
            with self.subTest(output=output):
                with self.assertRaises(planner.PlannerError):
                    planner.parse_revision_name_status(output, self.repo_root)

    def test_config_path_selects_core_schema_and_cli_surface(self) -> None:
        plan = self.plan_json(
            "--file", "codex-rs/core/src/config/mod.rs", "--mode", "standard"
        )
        commands = self.command_lines(plan)
        self.assertEqual("full", plan["telemetry_level"])
        self.assertIn("codex-core", plan["selected_packages"])
        self.assertIn("cli", plan["selected_surfaces"])
        self.assertEqual([], plan["warnings"])
        self.assertNotIn(["just", "fmt"], commands)
        self.assertNotIn(["just", "write-config-schema"], commands)
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "check",
                "-p",
                "codex-cli",
                "--bin",
                "codex",
            ],
            commands,
        )
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "test",
                "-p",
                "codex-core",
                "--no-run",
            ],
            commands,
        )
        prep_plan = self.action_json(
            "prep-plan",
            "--file",
            "codex-rs/core/src/config/mod.rs",
            "--mode",
            "standard",
        )
        prep_commands = self.command_lines(prep_plan)
        self.assertEqual("prep", prep_plan["stage"])
        self.assertIn(["just", "fmt"], prep_commands)
        self.assertIn(["just", "write-config-schema"], prep_commands)
        self.assertNotIn(
            ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-core"],
            prep_commands,
        )
        self.assertLess(
            prep_commands.index(["just", "fmt"]),
            prep_commands.index(["just", "write-config-schema"]),
        )
        support_bins_argv = [
            "./scripts/cargo-guard.sh",
            "cargo",
            "build",
            "-p",
            "codex-cli",
            "--bin",
            "codex",
            "-p",
            "codex-code-mode-host",
            "--bin",
            "codex-code-mode-host",
            "-p",
            "codex-rmcp-client",
            "--bin",
            "test_stdio_server",
            "--bin",
            "test_streamable_http_server",
            "-p",
            "codex-exec",
            "--bin",
            "codex-exec",
            "-p",
            "codex-linux-sandbox",
            "--bin",
            "codex-linux-sandbox",
            "-p",
            "codex-shell-escalation",
            "--bin",
            "codex-execve-wrapper",
        ]
        runtime_argv = ["./scripts/cargo-guard.sh", "cargo", "test", "-p", "codex-core"]
        self.assertIn(support_bins_argv, commands)
        self.assertIn(runtime_argv, commands)
        self.assertEqual(
            commands.index(support_bins_argv) + 1, commands.index(runtime_argv)
        )
        self.assertNotIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "test",
                "-p",
                "codex-core",
                "--",
                "--test-threads=4",
            ],
            commands,
        )
        check_command = self.command_for_argv(
            plan, ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-core"]
        )
        self.assertEqual("check", check_command["env"]["CARGO_GUARD_RESOURCE_PROFILE"])  # type: ignore[index]
        support_bins_command = self.command_for_argv(plan, support_bins_argv)
        self.assertEqual(
            "build", support_bins_command["env"]["CARGO_GUARD_RESOURCE_PROFILE"]
        )  # type: ignore[index]
        self.assertEqual("host", support_bins_command["codex_v8_target"])
        self.assertEqual("1", support_bins_command["env"]["CARGO_GUARD_NO_POST_CLEAN"])  # type: ignore[index]
        runtime_command = self.command_for_argv(plan, runtime_argv)
        self.assertEqual(
            "package_test", runtime_command["env"]["CARGO_GUARD_RESOURCE_PROFILE"]
        )  # type: ignore[index]
        self.assertEqual("1", runtime_command["env"]["CARGO_GUARD_TEST_THREADS_MAX"])  # type: ignore[index]
        self.assertEqual(
            "1", runtime_command["env"]["CARGO_GUARD_LOW_DISK_TEST_THREADS_MAX"]
        )  # type: ignore[index]
        self.assertEqual("1", runtime_command["env"]["CARGO_GUARD_NO_CLEAN"])  # type: ignore[index]
        self.assertEqual("0", runtime_command["env"]["CARGO_GUARD_EXPECTED_GROWTH_GIB"])  # type: ignore[index]
        self.assertEqual(0, runtime_command["effective_expected_growth_gib"])
        self.assertEqual(
            "forced:post-support-bins", runtime_command["expected_growth_source"]
        )

    def test_app_server_runtime_test_builds_first_party_support_binary_first(
        self,
    ) -> None:
        plan = self.plan_json(
            "--file",
            "codex-rs/app-server/tests/common/test_app_server.rs",
            "--mode",
            "standard",
        )
        commands = self.command_lines(plan)
        support_bins_argv = [
            "./scripts/cargo-guard.sh",
            "cargo",
            "build",
            "-p",
            "codex-cli",
            "--bin",
            "codex",
            "-p",
            "codex-code-mode-host",
            "--bin",
            "codex-code-mode-host",
            "-p",
            "codex-rmcp-client",
            "--bin",
            "test_stdio_server",
            "--bin",
            "test_streamable_http_server",
            "-p",
            "codex-exec",
            "--bin",
            "codex-exec",
            "-p",
            "codex-linux-sandbox",
            "--bin",
            "codex-linux-sandbox",
            "-p",
            "codex-shell-escalation",
            "--bin",
            "codex-execve-wrapper",
        ]
        runtime_argv = [
            "./scripts/cargo-guard.sh",
            "cargo",
            "test",
            "-p",
            "codex-app-server",
        ]
        self.assertIn("codex-app-server", plan["selected_packages"])
        self.assertIn(support_bins_argv, commands)
        self.assertIn(runtime_argv, commands)
        self.assertEqual(
            commands.index(support_bins_argv) + 1, commands.index(runtime_argv)
        )
        support_bins_command = self.command_for_argv(plan, support_bins_argv)
        self.assertEqual("host", support_bins_command["codex_v8_target"])
        runtime_command = self.command_for_argv(plan, runtime_argv)
        self.assertEqual("1", runtime_command["env"]["CARGO_GUARD_NO_CLEAN"])  # type: ignore[index]
        self.assertEqual("0", runtime_command["env"]["CARGO_GUARD_EXPECTED_GROWTH_GIB"])  # type: ignore[index]
        self.assertEqual(0, runtime_command["effective_expected_growth_gib"])
        self.assertEqual(
            "forced:post-support-bins", runtime_command["expected_growth_source"]
        )

    def test_structural_commands_run_before_expensive_package_ladder(self) -> None:
        plan = self.plan_json(
            "--file",
            "codex-rs/Cargo.lock",
            "--file",
            "codex-rs/core/src/config/mod.rs",
            "--mode",
            "standard",
        )
        commands = self.command_lines(plan)
        self.assertLess(
            commands.index(["just", "bazel-lock-check"]),
            commands.index(
                ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-core"]
            ),
        )
        self.assertNotIn(["just", "write-config-schema"], commands)
        self.assertNotIn(["just", "bazel-lock-update"], commands)

    def test_manifest_change_splits_prep_lock_update_from_validation_check(
        self,
    ) -> None:
        validation_plan = self.plan_json(
            "--file", "codex-rs/Cargo.toml", "--mode", "standard"
        )
        validation_commands = self.command_lines(validation_plan)
        self.assertEqual("validation", validation_plan["stage"])
        self.assertIn(["just", "bazel-lock-check"], validation_commands)
        self.assertNotIn(["just", "fmt"], validation_commands)
        self.assertNotIn(["just", "bazel-lock-update"], validation_commands)

        prep_plan = self.action_json(
            "prep-plan", "--file", "codex-rs/Cargo.toml", "--mode", "standard"
        )
        prep_commands = self.command_lines(prep_plan)
        self.assertEqual("prep", prep_plan["stage"])
        self.assertIn(["just", "fmt"], prep_commands)
        self.assertIn(["just", "bazel-lock-update"], prep_commands)
        self.assertNotIn(["just", "bazel-lock-check"], prep_commands)
        self.assertNotIn(
            ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-core"],
            prep_commands,
        )

        non_root_validation_plan = self.plan_json(
            "--file", "codex-rs/core/Cargo.toml", "--mode", "standard"
        )
        non_root_validation_commands = self.command_lines(non_root_validation_plan)
        self.assertEqual("validation", non_root_validation_plan["stage"])
        self.assertIn(["just", "bazel-lock-check"], non_root_validation_commands)
        self.assertNotIn(["just", "fmt"], non_root_validation_commands)
        self.assertNotIn(["just", "bazel-lock-update"], non_root_validation_commands)

        non_root_prep_plan = self.action_json(
            "prep-plan", "--file", "codex-rs/core/Cargo.toml", "--mode", "standard"
        )
        non_root_prep_commands = self.command_lines(non_root_prep_plan)
        self.assertEqual("prep", non_root_prep_plan["stage"])
        self.assertIn(["just", "fmt"], non_root_prep_commands)
        self.assertIn(["just", "bazel-lock-update"], non_root_prep_commands)
        self.assertNotIn(["just", "bazel-lock-check"], non_root_prep_commands)

    def test_codex_core_runtime_test_ignores_stale_growth_history_after_support_prebuilds(
        self,
    ) -> None:
        initial_plan = self.plan_json(
            "--file", "codex-rs/core/src/config/mod.rs", "--mode", "standard"
        )
        initial_runtime_command = self.command_for_argv(
            initial_plan,
            ["./scripts/cargo-guard.sh", "cargo", "test", "-p", "codex-core"],
        )
        receipt_dir = self.repo_root / ".validation-receipts"
        receipt_dir.mkdir()
        (receipt_dir / "history.jsonl").write_text(
            json.dumps(
                {
                    "disk_emergency": False,
                    "fingerprint": initial_runtime_command["fingerprint"],
                    "observed_growth_gib": 6,
                    "resource_profile": "package_test",
                    "risk_kind": "success",
                    "status": 0,
                }
            )
            + "\n"
        )

        plan = self.plan_json(
            "--file",
            "codex-rs/core/src/config/mod.rs",
            "--mode",
            "standard",
            receipt_dir=receipt_dir,
        )
        runtime_command = self.command_for_argv(
            plan,
            ["./scripts/cargo-guard.sh", "cargo", "test", "-p", "codex-core"],
        )

        self.assertEqual("0", runtime_command["env"]["CARGO_GUARD_EXPECTED_GROWTH_GIB"])  # type: ignore[index]
        self.assertEqual("1", runtime_command["env"]["CARGO_GUARD_NO_CLEAN"])  # type: ignore[index]
        self.assertEqual(0, runtime_command["effective_expected_growth_gib"])
        self.assertEqual(
            "forced:post-support-bins", runtime_command["expected_growth_source"]
        )

    def test_tui_path_selects_snapshot_note_and_cli_surface(self) -> None:
        plan = self.plan_json("--file", "codex-rs/tui/src/app.rs", "--mode", "standard")
        commands = self.command_lines(plan)
        self.assertIn("codex-tui", plan["selected_packages"])
        self.assertIn("cli", plan["selected_surfaces"])
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "check",
                "-p",
                "codex-cli",
                "--bin",
                "codex",
            ],
            commands,
        )
        manual_messages = [entry["message"] for entry in plan["manual"]]  # type: ignore[index]
        self.assertIn(
            "Review generated *.snap.new files before running cargo insta accept.",
            manual_messages,
        )

    def test_explicit_cli_strict_selects_strict_and_smoke_recipes(self) -> None:
        plan = self.plan_json("--surface", "cli", "--mode", "strict")
        commands = self.command_lines(plan)
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "check",
                "-p",
                "codex-cli",
                "--bin",
                "codex",
            ],
            commands,
        )
        self.assertIn(["just", "strict-codex-bin"], commands)
        self.assertIn(["just", "smoke-codex-bin"], commands)

    def test_features_and_tools_paths_have_explicit_cli_runtime_rules(self) -> None:
        plan = self.plan_json(
            "--file",
            "codex-rs/features/src/lib.rs",
            "--file",
            "codex-rs/protocol/src/permissions.rs",
            "--file",
            "codex-rs/tools/src/tool_config.rs",
            "--mode",
            "strict",
        )
        commands = self.command_lines(plan)
        self.assertEqual([], plan["warnings"])
        self.assertIn("cli", plan["selected_surfaces"])
        self.assertIn("codex-features", plan["selected_packages"])
        self.assertIn("codex-protocol", plan["selected_packages"])
        self.assertIn("codex-tools", plan["selected_packages"])
        self.assertIn(
            ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-features"],
            commands,
        )
        self.assertIn(
            ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-protocol"],
            commands,
        )
        self.assertIn(
            ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-tools"],
            commands,
        )
        self.assertIn(["just", "clippy-strict", "-p", "codex-features"], commands)
        self.assertIn(["just", "clippy-strict", "-p", "codex-protocol"], commands)
        self.assertIn(["just", "clippy-strict", "-p", "codex-tools"], commands)
        self.assertIn(["just", "strict-codex-bin"], commands)

    def test_cli_runtime_leaf_paths_have_explicit_rules(self) -> None:
        plan = self.plan_json(
            "--file",
            "codex-rs/analytics/src/reducer.rs",
            "--file",
            "codex-rs/codex-mcp/src/mcp/mod.rs",
            "--file",
            "codex-rs/file-system/src/lib.rs",
            "--mode",
            "strict",
        )
        commands = self.command_lines(plan)
        self.assertEqual([], plan["warnings"])
        self.assertIn("cli", plan["selected_surfaces"])
        self.assertIn("codex-analytics", plan["selected_packages"])
        self.assertIn("codex-mcp", plan["selected_packages"])
        self.assertIn("codex-file-system", plan["selected_packages"])
        self.assertIn(
            ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-analytics"],
            commands,
        )
        self.assertIn(
            ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-mcp"], commands
        )
        self.assertIn(
            ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-file-system"],
            commands,
        )
        self.assertIn(["just", "clippy-strict", "-p", "codex-analytics"], commands)
        self.assertIn(["just", "clippy-strict", "-p", "codex-mcp"], commands)
        self.assertIn(["just", "clippy-strict", "-p", "codex-file-system"], commands)
        self.assertIn(["just", "strict-codex-bin"], commands)

    def test_thread_manager_sample_path_has_explicit_cli_rule(self) -> None:
        plan = self.plan_json(
            "--file", "codex-rs/thread-manager-sample/src/main.rs", "--mode", "standard"
        )
        self.assertEqual([], plan["warnings"])
        self.assertIn("codex-thread-manager-sample", plan["selected_packages"])
        self.assertIn("cli", plan["selected_surfaces"])

    def test_test_binary_support_path_has_explicit_cli_rule(self) -> None:
        plan = self.plan_json(
            "--file", "codex-rs/test-binary-support/lib.rs", "--mode", "strict"
        )
        commands = self.command_lines(plan)
        self.assertEqual([], plan["warnings"])
        self.assertIn("codex-test-binary-support", plan["selected_packages"])
        self.assertIn("cli", plan["selected_surfaces"])
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "check",
                "-p",
                "codex-test-binary-support",
            ],
            commands,
        )
        self.assertIn(
            ["just", "clippy-strict", "-p", "codex-test-binary-support"], commands
        )
        self.assertIn(["just", "strict-codex-bin"], commands)

    def test_process_crate_paths_have_explicit_cli_runtime_rule(self) -> None:
        paths = (
            "codex-rs/utils/process/Cargo.toml",
            "codex-rs/utils/process/src/lib.rs",
            "codex-rs/utils/process/src/process_tests.rs",
        )

        for file_path in paths:
            with self.subTest(file_path=file_path):
                plan = self.plan_json("--file", file_path, "--mode", "strict")
                commands = self.command_lines(plan)
                self.assertEqual([], plan["warnings"])
                self.assertIn("codex-utils-process", plan["selected_packages"])
                self.assertIn("cli", plan["selected_surfaces"])
                self.assertIn("runtime", plan["flags"])
                self.assertIn(
                    [
                        "./scripts/cargo-guard.sh",
                        "cargo",
                        "check",
                        "-p",
                        "codex-utils-process",
                    ],
                    commands,
                )
                self.assertIn(
                    ["just", "clippy-strict", "-p", "codex-utils-process"],
                    commands,
                )
                self.assertIn(["just", "strict-codex-bin"], commands)

    def test_core_api_path_has_explicit_cli_rule(self) -> None:
        plan = self.plan_json(
            "--file", "codex-rs/core-api/src/lib.rs", "--mode", "standard"
        )
        commands = self.command_lines(plan)
        self.assertEqual([], plan["warnings"])
        self.assertIn("codex-core-api", plan["selected_packages"])
        self.assertIn("cli", plan["selected_surfaces"])
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "check",
                "-p",
                "codex-core-api",
            ],
            commands,
        )
        self.assertNotIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "check",
                "-p",
                "codex-core-api",
                "--tests",
            ],
            commands,
        )
        self.assertNotIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "test",
                "-p",
                "codex-core-api",
                "--no-run",
            ],
            commands,
        )
        self.assertNotIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "test",
                "-p",
                "codex-core-api",
            ],
            commands,
        )

    def test_metadata_requires_target_capabilities(self) -> None:
        metadata_path = self.repo_root / "missing-target-capabilities.json"
        metadata_path.write_text(
            json.dumps(
                {
                    "packages": [
                        {
                            "name": "codex-core-api",
                            "manifest_path": str(
                                self.repo_root / "codex-rs" / "core-api" / "Cargo.toml"
                            ),
                        }
                    ]
                }
            )
        )

        process = self.run_planner(
            "plan",
            "--file",
            "codex-rs/core-api/src/lib.rs",
            "--mode",
            "standard",
            "--no-receipt",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
            check=False,
        )

        self.assertEqual(2, process.returncode)
        self.assertIn(
            "cargo metadata package 'codex-core-api' targets must be a non-empty list",
            process.stderr,
        )

    def test_app_server_transport_path_has_explicit_app_server_runtime_rule(
        self,
    ) -> None:
        plan = self.plan_json(
            "--file",
            "codex-rs/app-server-transport/src/lib.rs",
            "--mode",
            "standard",
        )
        commands = self.command_lines(plan)
        self.assertEqual([], plan["warnings"])
        self.assertIn("codex-app-server-transport", plan["selected_packages"])
        self.assertIn("app_server", plan["selected_surfaces"])
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "check",
                "-p",
                "codex-app-server-transport",
            ],
            commands,
        )
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "test",
                "-p",
                "codex-app-server-transport",
                "--no-run",
            ],
            commands,
        )
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "check",
                "-p",
                "codex-app-server-transport",
                "--tests",
            ],
            commands,
        )
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "test",
                "-p",
                "codex-app-server-transport",
            ],
            commands,
        )

    def test_cloud_config_path_has_explicit_cli_runtime_rule(self) -> None:
        plan = self.plan_json(
            "--file",
            "codex-rs/cloud-config/src/lib.rs",
            "--mode",
            "standard",
        )
        commands = self.command_lines(plan)
        self.assertEqual([], plan["warnings"])
        self.assertIn("codex-cloud-config", plan["selected_packages"])
        self.assertIn("cli", plan["selected_surfaces"])
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "check",
                "-p",
                "codex-cloud-config",
            ],
            commands,
        )
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "test",
                "-p",
                "codex-cloud-config",
            ],
            commands,
        )
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "check",
                "-p",
                "codex-cli",
                "--bin",
                "codex",
            ],
            commands,
        )

    def test_doctest_only_package_keeps_runtime_without_test_target_build(self) -> None:
        metadata = json.loads(self.metadata_path.read_text())
        cloud_config = next(
            package
            for package in metadata["packages"]
            if package["name"] == "codex-cloud-config"
        )
        cloud_config["targets"] = [{"test": False, "doctest": True}]
        self.metadata_path.write_text(json.dumps(metadata))

        plan = self.plan_json(
            "--file",
            "codex-rs/cloud-config/src/lib.rs",
            "--mode",
            "standard",
        )
        commands = self.command_lines(plan)

        self.assertNotIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "check",
                "-p",
                "codex-cloud-config",
                "--tests",
            ],
            commands,
        )
        self.assertNotIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "test",
                "-p",
                "codex-cloud-config",
                "--no-run",
            ],
            commands,
        )
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "test",
                "-p",
                "codex-cloud-config",
            ],
            commands,
        )

    def test_known_cli_fallback_roots_have_explicit_strict_rule(self) -> None:
        cases = (
            ("codex-code-mode-protocol", "codex-rs/code-mode-protocol/Cargo.toml"),
            (
                "codex-exec-server-protocol",
                "codex-rs/exec-server-protocol/Cargo.toml",
            ),
            ("codex-feedback", "codex-rs/feedback/Cargo.toml"),
            ("codex-install-context", "codex-rs/install-context/Cargo.toml"),
            ("codex-models-manager", "codex-rs/models-manager/src/manager.rs"),
            ("codex-websocket-client", "codex-rs/websocket-client/Cargo.toml"),
        )

        for package, file_path in cases:
            with self.subTest(package=package):
                plan = self.plan_json("--file", file_path, "--mode", "strict")
                commands = self.command_lines(plan)
                self.assertEqual([], plan["warnings"])
                self.assertIn(package, plan["selected_packages"])
                self.assertIn("cli", plan["selected_surfaces"])
                self.assertIn(
                    [
                        "./scripts/cargo-guard.sh",
                        "cargo",
                        "check",
                        "-p",
                        package,
                    ],
                    commands,
                )
                self.assertIn(["just", "clippy-strict", "-p", package], commands)
                self.assertIn(["just", "strict-codex-bin"], commands)

    def test_feature_sensitive_rust_path_fails_closed_without_profile(self) -> None:
        process = self.run_planner(
            "plan",
            "--file",
            "codex-rs/core/src/feature.rs",
            "--mode",
            "standard",
            "--no-receipt",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
            check=False,
        )
        self.assertNotEqual(0, process.returncode)
        self.assertIn("package_features profile", process.stderr)

    def test_feature_cfg_shapes_fail_closed_without_profile(self) -> None:
        for file_path in (
            "codex-rs/core/src/feature_all.rs",
            "codex-rs/core/src/feature_any.rs",
            "codex-rs/core/src/feature_attr.rs",
            "codex-rs/core/src/feature_macro.rs",
        ):
            with self.subTest(file_path=file_path):
                process = self.run_planner(
                    "plan",
                    "--file",
                    file_path,
                    "--mode",
                    "standard",
                    "--no-receipt",
                    "--repo-root",
                    str(self.repo_root),
                    "--metadata-json",
                    str(self.metadata_path),
                    "--config",
                    str(PRODUCTION_CONFIG),
                    check=False,
                )
                self.assertNotEqual(0, process.returncode)
                self.assertIn("package_features profile", process.stderr)

    def test_hunk_scoped_feature_detection_ignores_untouched_old_feature_gate(
        self,
    ) -> None:
        feature_path = (
            self.repo_root / "codex-rs" / "core" / "src" / "tracked_feature.rs"
        )
        feature_path.write_text(
            '#[cfg(feature = "old")]\n'
            "pub fn value() -> i32 { 1 }\n\n"
            "pub fn default_value() -> i32 { 1 }\n"
        )
        subprocess.run(
            ["git", "init"], cwd=self.repo_root, stdout=subprocess.DEVNULL, check=True
        )
        subprocess.run(["git", "add", "."], cwd=self.repo_root, check=True)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
            cwd=self.repo_root,
            stdout=subprocess.DEVNULL,
            check=True,
        )
        feature_path.write_text(
            '#[cfg(feature = "old")]\n'
            "pub fn value() -> i32 { 1 }\n\n"
            "pub fn default_value() -> i32 { 2 }\n"
        )
        plan = self.plan_json(
            "--file", "codex-rs/core/src/tracked_feature.rs", "--mode", "standard"
        )
        self.assertIn("codex-core", plan["selected_packages"])

    def test_hunk_scoped_feature_detection_fails_on_feature_gate_deletion(self) -> None:
        feature_path = (
            self.repo_root / "codex-rs" / "core" / "src" / "deleted_feature.rs"
        )
        feature_path.write_text(
            '#[cfg(feature = "old")]\npub fn value() -> i32 { 1 }\n'
        )
        subprocess.run(
            ["git", "init"], cwd=self.repo_root, stdout=subprocess.DEVNULL, check=True
        )
        subprocess.run(["git", "add", "."], cwd=self.repo_root, check=True)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
            cwd=self.repo_root,
            stdout=subprocess.DEVNULL,
            check=True,
        )
        feature_path.write_text("pub fn value() -> i32 { 1 }\n")
        process = self.run_planner(
            "plan",
            "--file",
            "codex-rs/core/src/deleted_feature.rs",
            "--mode",
            "standard",
            "--no-receipt",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
            check=False,
        )
        self.assertNotEqual(0, process.returncode)
        self.assertIn("package_features profile", process.stderr)

    def test_hunk_scoped_feature_region_detection_fails_inside_gated_item(self) -> None:
        feature_path = (
            self.repo_root / "codex-rs" / "core" / "src" / "feature_region.rs"
        )
        feature_path.write_text(
            '#[cfg(feature = "oauth")]\n'
            "pub fn build_oauth_client() {\n"
            "    let value = 1;\n"
            "}\n\n"
            "pub fn default_client() {\n"
            "    let value = 1;\n"
            "}\n"
        )
        subprocess.run(
            ["git", "init"], cwd=self.repo_root, stdout=subprocess.DEVNULL, check=True
        )
        subprocess.run(["git", "add", "."], cwd=self.repo_root, check=True)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
            cwd=self.repo_root,
            stdout=subprocess.DEVNULL,
            check=True,
        )
        feature_path.write_text(
            '#[cfg(feature = "oauth")]\n'
            "pub fn build_oauth_client() {\n"
            "    let value = 2;\n"
            "}\n\n"
            "pub fn default_client() {\n"
            "    let value = 1;\n"
            "}\n"
        )
        process = self.run_planner(
            "plan",
            "--file",
            "codex-rs/core/src/feature_region.rs",
            "--mode",
            "standard",
            "--no-receipt",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
            check=False,
        )
        self.assertNotEqual(0, process.returncode)
        self.assertIn(
            "changed hunk intersects cfg(feature = ...) region", process.stderr
        )

    def test_hunk_scoped_feature_region_detection_fails_inside_nested_gated_item(
        self,
    ) -> None:
        feature_path = (
            self.repo_root / "codex-rs" / "core" / "src" / "feature_region_nested.rs"
        )
        feature_path.write_text(
            '#[cfg(all(not(windows), feature = "oauth"))]\n'
            "pub fn build_oauth_client() {\n"
            "    let value = 1;\n"
            "}\n\n"
            "pub fn default_client() {\n"
            "    let value = 1;\n"
            "}\n"
        )
        subprocess.run(
            ["git", "init"], cwd=self.repo_root, stdout=subprocess.DEVNULL, check=True
        )
        subprocess.run(["git", "add", "."], cwd=self.repo_root, check=True)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
            cwd=self.repo_root,
            stdout=subprocess.DEVNULL,
            check=True,
        )
        feature_path.write_text(
            '#[cfg(all(not(windows), feature = "oauth"))]\n'
            "pub fn build_oauth_client() {\n"
            "    let value = 2;\n"
            "}\n\n"
            "pub fn default_client() {\n"
            "    let value = 1;\n"
            "}\n"
        )
        process = self.run_planner(
            "plan",
            "--file",
            "codex-rs/core/src/feature_region_nested.rs",
            "--mode",
            "standard",
            "--no-receipt",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
            check=False,
        )
        self.assertNotEqual(0, process.returncode)
        self.assertIn(
            "changed hunk intersects cfg(feature = ...) region", process.stderr
        )

    def test_hunk_scoped_feature_region_detection_fails_inside_deep_nested_gated_item(
        self,
    ) -> None:
        feature_path = (
            self.repo_root
            / "codex-rs"
            / "core"
            / "src"
            / "feature_region_deep_nested.rs"
        )
        feature_path.write_text(
            '#[cfg(all(any(unix, target_os = "linux"), feature = "oauth"))]\n'
            "pub fn build_oauth_client() {\n"
            "    let value = 1;\n"
            "}\n\n"
            "pub fn default_client() {\n"
            "    let value = 1;\n"
            "}\n"
        )
        subprocess.run(
            ["git", "init"], cwd=self.repo_root, stdout=subprocess.DEVNULL, check=True
        )
        subprocess.run(["git", "add", "."], cwd=self.repo_root, check=True)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
            cwd=self.repo_root,
            stdout=subprocess.DEVNULL,
            check=True,
        )
        feature_path.write_text(
            '#[cfg(all(any(unix, target_os = "linux"), feature = "oauth"))]\n'
            "pub fn build_oauth_client() {\n"
            "    let value = 2;\n"
            "}\n\n"
            "pub fn default_client() {\n"
            "    let value = 1;\n"
            "}\n"
        )
        process = self.run_planner(
            "plan",
            "--file",
            "codex-rs/core/src/feature_region_deep_nested.rs",
            "--mode",
            "standard",
            "--no-receipt",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
            check=False,
        )
        self.assertNotEqual(0, process.returncode)
        self.assertIn(
            "changed hunk intersects cfg(feature = ...) region", process.stderr
        )

    def test_hunk_scoped_feature_region_detection_fails_inside_multiline_gated_item(
        self,
    ) -> None:
        feature_path = (
            self.repo_root / "codex-rs" / "core" / "src" / "feature_region_multiline.rs"
        )
        feature_path.write_text(
            "#[cfg(all(\n"
            "    unix,\n"
            '    feature = "oauth",\n'
            "))]\n"
            "pub fn build_oauth_client() {\n"
            "    let value = 1;\n"
            "}\n\n"
            "pub fn default_client() {\n"
            "    let value = 1;\n"
            "}\n"
        )
        subprocess.run(
            ["git", "init"], cwd=self.repo_root, stdout=subprocess.DEVNULL, check=True
        )
        subprocess.run(["git", "add", "."], cwd=self.repo_root, check=True)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
            cwd=self.repo_root,
            stdout=subprocess.DEVNULL,
            check=True,
        )
        feature_path.write_text(
            "#[cfg(all(\n"
            "    unix,\n"
            '    feature = "oauth",\n'
            "))]\n"
            "pub fn build_oauth_client() {\n"
            "    let value = 2;\n"
            "}\n\n"
            "pub fn default_client() {\n"
            "    let value = 1;\n"
            "}\n"
        )
        process = self.run_planner(
            "plan",
            "--file",
            "codex-rs/core/src/feature_region_multiline.rs",
            "--mode",
            "standard",
            "--no-receipt",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
            check=False,
        )
        self.assertNotEqual(0, process.returncode)
        self.assertIn(
            "changed hunk intersects cfg(feature = ...) region", process.stderr
        )

    def test_hunk_scoped_feature_region_detection_fails_inside_nested_cfg_attr_item(
        self,
    ) -> None:
        feature_path = (
            self.repo_root
            / "codex-rs"
            / "core"
            / "src"
            / "feature_region_attr_nested.rs"
        )
        feature_path.write_text(
            '#[cfg_attr(all(not(windows), feature = "oauth"), derive(Debug))]\n'
            "pub struct OAuthClient {\n"
            "    value: i32,\n"
            "}\n\n"
            "pub struct DefaultClient {\n"
            "    value: i32,\n"
            "}\n"
        )
        subprocess.run(
            ["git", "init"], cwd=self.repo_root, stdout=subprocess.DEVNULL, check=True
        )
        subprocess.run(["git", "add", "."], cwd=self.repo_root, check=True)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
            cwd=self.repo_root,
            stdout=subprocess.DEVNULL,
            check=True,
        )
        feature_path.write_text(
            '#[cfg_attr(all(not(windows), feature = "oauth"), derive(Debug))]\n'
            "pub struct OAuthClient {\n"
            "    value: i64,\n"
            "}\n\n"
            "pub struct DefaultClient {\n"
            "    value: i32,\n"
            "}\n"
        )
        process = self.run_planner(
            "plan",
            "--file",
            "codex-rs/core/src/feature_region_attr_nested.rs",
            "--mode",
            "standard",
            "--no-receipt",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
            check=False,
        )
        self.assertNotEqual(0, process.returncode)
        self.assertIn(
            "changed hunk intersects cfg(feature = ...) region", process.stderr
        )

    def test_hunk_scoped_feature_region_detection_fails_inside_multiline_cfg_attr_item(
        self,
    ) -> None:
        feature_path = (
            self.repo_root
            / "codex-rs"
            / "core"
            / "src"
            / "feature_region_attr_multiline.rs"
        )
        feature_path.write_text(
            "#[cfg_attr(\n"
            '    all(not(windows), feature = "oauth"),\n'
            "    derive(Debug),\n"
            ")]\n"
            "pub struct OAuthClient {\n"
            "    value: i32,\n"
            "}\n\n"
            "pub struct DefaultClient {\n"
            "    value: i32,\n"
            "}\n"
        )
        subprocess.run(
            ["git", "init"], cwd=self.repo_root, stdout=subprocess.DEVNULL, check=True
        )
        subprocess.run(["git", "add", "."], cwd=self.repo_root, check=True)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
            cwd=self.repo_root,
            stdout=subprocess.DEVNULL,
            check=True,
        )
        feature_path.write_text(
            "#[cfg_attr(\n"
            '    all(not(windows), feature = "oauth"),\n'
            "    derive(Debug),\n"
            ")]\n"
            "pub struct OAuthClient {\n"
            "    value: i64,\n"
            "}\n\n"
            "pub struct DefaultClient {\n"
            "    value: i32,\n"
            "}\n"
        )
        process = self.run_planner(
            "plan",
            "--file",
            "codex-rs/core/src/feature_region_attr_multiline.rs",
            "--mode",
            "standard",
            "--no-receipt",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
            check=False,
        )
        self.assertNotEqual(0, process.returncode)
        self.assertIn(
            "changed hunk intersects cfg(feature = ...) region", process.stderr
        )

    def test_hunk_scoped_feature_region_detection_ignores_outside_gated_item(
        self,
    ) -> None:
        feature_path = (
            self.repo_root / "codex-rs" / "core" / "src" / "feature_region_outside.rs"
        )
        feature_path.write_text(
            '#[cfg(feature = "oauth")]\n'
            "pub fn build_oauth_client() {\n"
            "    let value = 1;\n"
            "}\n\n"
            "pub fn default_client() {\n"
            "    let value = 1;\n"
            "}\n"
        )
        subprocess.run(
            ["git", "init"], cwd=self.repo_root, stdout=subprocess.DEVNULL, check=True
        )
        subprocess.run(["git", "add", "."], cwd=self.repo_root, check=True)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
            cwd=self.repo_root,
            stdout=subprocess.DEVNULL,
            check=True,
        )
        feature_path.write_text(
            '#[cfg(feature = "oauth")]\n'
            "pub fn build_oauth_client() {\n"
            "    let value = 1;\n"
            "}\n\n"
            "pub fn default_client() {\n"
            "    let value = 2;\n"
            "}\n"
        )
        plan = self.plan_json(
            "--file",
            "codex-rs/core/src/feature_region_outside.rs",
            "--mode",
            "standard",
        )
        self.assertIn("codex-core", plan["selected_packages"])

    def test_hunk_scoped_feature_region_detection_fails_on_old_region_deletion(
        self,
    ) -> None:
        feature_path = (
            self.repo_root / "codex-rs" / "core" / "src" / "feature_region_deleted.rs"
        )
        feature_path.write_text(
            '#[cfg(feature = "oauth")]\n'
            "pub fn build_oauth_client() {\n"
            "    let value = 1;\n"
            "}\n"
        )
        subprocess.run(
            ["git", "init"], cwd=self.repo_root, stdout=subprocess.DEVNULL, check=True
        )
        subprocess.run(["git", "add", "."], cwd=self.repo_root, check=True)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
            cwd=self.repo_root,
            stdout=subprocess.DEVNULL,
            check=True,
        )
        feature_path.write_text(
            '#[cfg(feature = "oauth")]\npub fn build_oauth_client() {\n}\n'
        )
        process = self.run_planner(
            "plan",
            "--file",
            "codex-rs/core/src/feature_region_deleted.rs",
            "--mode",
            "standard",
            "--no-receipt",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
            check=False,
        )
        self.assertNotEqual(0, process.returncode)
        self.assertIn(
            "changed hunk intersects cfg(feature = ...) region", process.stderr
        )

    def test_hunk_scoped_manifest_features_deletion_fails_closed(self) -> None:
        manifest_path = self.repo_root / "codex-rs" / "core" / "Cargo.toml"
        manifest_path.write_text(
            '[package]\nname = "codex-core"\n\n[features]\nold = []\n'
        )
        subprocess.run(
            ["git", "init"], cwd=self.repo_root, stdout=subprocess.DEVNULL, check=True
        )
        subprocess.run(["git", "add", "."], cwd=self.repo_root, check=True)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
            cwd=self.repo_root,
            stdout=subprocess.DEVNULL,
            check=True,
        )
        manifest_path.write_text('[package]\nname = "codex-core"\n')
        process = self.run_planner(
            "plan",
            "--file",
            "codex-rs/core/Cargo.toml",
            "--mode",
            "standard",
            "--no-receipt",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
            check=False,
        )
        self.assertNotEqual(0, process.returncode)
        self.assertIn("package_features profile", process.stderr)

    def test_default_receipt_dir_is_sangoi_validation(self) -> None:
        process = self.run_planner(
            "plan",
            "--json",
            "--file",
            "justfile",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
        )
        plan = json.loads(process.stdout)
        receipt_dir = self.repo_root / ".sangoi" / "validation"
        self.assertEqual(str(receipt_dir), plan["receipt_dir"])
        self.assertTrue((receipt_dir / "last-plan.json").is_file())

    def test_config_validation_rejects_unknown_references(self) -> None:
        cases = {
            "unknown package": '[[path_rules]]\npatterns = ["fixture.txt"]\npackages = ["missing"]\n',
            "unknown surface": '[[path_rules]]\npatterns = ["fixture.txt"]\nsurfaces = ["missing"]\n',
            "unknown command": '[[path_rules]]\npatterns = ["fixture.txt"]\ncommands = ["missing"]\n',
            "unknown generator": '[[path_rules]]\npatterns = ["fixture.txt"]\ngenerators = ["missing"]\n',
            "unknown profile": '[commands.bad]\nargv = ["true"]\nprofile = "missing"\n',
        }
        for label, extra_config in cases.items():
            with self.subTest(label=label):
                config_path = self.repo_root / f"{label.replace(' ', '-')}.toml"
                config_path.write_text(
                    textwrap.dedent(f"""
                    schema_version = 1

                    [defaults]
                    standard_mode = "standard"
                    unknown_rust_path_policy = "package-plus-cli-strict"
                    workspace_features_policy = "deny-routine-all-features"

                    [receipts]
                    dir = "receipts"

                    {extra_config}
                """).strip()
                    + "\n"
                )
                (self.repo_root / "fixture.txt").write_text("fixture\n")
                process = self.run_planner(
                    "plan",
                    "--file",
                    "fixture.txt",
                    "--repo-root",
                    str(self.repo_root),
                    "--metadata-json",
                    str(self.metadata_path),
                    "--config",
                    str(config_path),
                    "--no-receipt",
                    check=False,
                )
                self.assertNotEqual(0, process.returncode)
                self.assertIn("unknown", process.stderr)

    def test_guard_harness_path_selects_guard_self_tests(self) -> None:
        plan = self.plan_json(
            "--file", "scripts/test-cargo-guard.sh", "--mode", "standard"
        )
        commands = self.command_lines(plan)
        self.assertIn(["bash", "-n", "scripts/test-cargo-guard.sh"], commands)
        self.assertIn(["just", "test-cargo-guard"], commands)

    def test_justfile_path_selects_cheap_entrypoint_checks(self) -> None:
        plan = self.plan_json("--file", "justfile", "--mode", "standard")
        commands = self.command_lines(plan)
        self.assertIn(["just", "--summary"], commands)
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "plan",
                "--file",
                "scripts/cargo-validate.py",
                "--mode",
                "standard",
                "--no-receipt",
            ],
            commands,
        )

    def test_nextest_config_path_selects_profile_shape_check_only(self) -> None:
        plan = self.plan_json(
            "--file", "codex-rs/.config/nextest.toml", "--mode", "standard"
        )
        commands = self.command_lines(plan)
        self.assertIn("nextest_config", plan["selected_surfaces"])
        self.assertTrue(any(command[:2] == ["python3", "-c"] for command in commands))
        self.assertNotIn(["./scripts/cargo-guard.sh", "cargo", "check"], commands)

    def test_mixed_file_unknown_rust_path_keeps_cli_surface(self) -> None:
        plan = self.plan_json(
            "--file",
            "codex-rs/.config/nextest.toml",
            "--file",
            "codex-rs/unmapped-fixture/src/lib.rs",
            "--mode",
            "standard",
        )
        commands = self.command_lines(plan)
        self.assertIn("nextest_config", plan["selected_surfaces"])
        self.assertIn("cli", plan["selected_surfaces"])
        self.assertTrue(
            any(
                "codex-rs/unmapped-fixture/src/lib.rs" in warning
                for warning in plan["warnings"]
            )
        )  # type: ignore[index]
        self.assertIn("codex-unmapped-fixture", plan["selected_packages"])
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "check",
                "-p",
                "codex-unmapped-fixture",
            ],
            commands,
        )
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "check",
                "-p",
                "codex-cli",
                "--bin",
                "codex",
            ],
            commands,
        )

    def test_mixed_file_unknown_package_manifest_keeps_cli_surface(self) -> None:
        plan = self.plan_json(
            "--file",
            "codex-rs/.config/nextest.toml",
            "--file",
            "codex-rs/unmapped-fixture/Cargo.toml",
            "--mode",
            "standard",
        )
        commands = self.command_lines(plan)
        self.assertIn("nextest_config", plan["selected_surfaces"])
        self.assertIn("cli", plan["selected_surfaces"])
        self.assertTrue(
            any(
                "codex-rs/unmapped-fixture/Cargo.toml" in warning
                for warning in plan["warnings"]
            )
        )  # type: ignore[index]
        self.assertIn("codex-unmapped-fixture", plan["selected_packages"])
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "check",
                "-p",
                "codex-unmapped-fixture",
            ],
            commands,
        )
        self.assertIn(
            [
                "./scripts/cargo-guard.sh",
                "cargo",
                "check",
                "-p",
                "codex-cli",
                "--bin",
                "codex",
            ],
            commands,
        )

    def test_unknown_package_fallback_prints_warning_and_strict_fails(self) -> None:
        process = self.run_planner(
            "plan",
            "--file",
            "codex-rs/.config/nextest.toml",
            "--file",
            "codex-rs/unmapped-fixture/Cargo.toml",
            "--mode",
            "standard",
            "--no-receipt",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
        )
        self.assertIn("warnings:", process.stdout)
        self.assertIn("codex-rs/unmapped-fixture/Cargo.toml", process.stdout)

        strict_process = self.run_planner(
            "plan",
            "--file",
            "codex-rs/.config/nextest.toml",
            "--file",
            "codex-rs/unmapped-fixture/Cargo.toml",
            "--mode",
            "strict",
            "--no-receipt",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
            check=False,
        )
        self.assertNotEqual(0, strict_process.returncode)
        self.assertIn(
            "strict mode requires explicit validation path rules", strict_process.stderr
        )
        self.assertIn("codex-rs/unmapped-fixture/Cargo.toml", strict_process.stderr)

    def test_known_package_roots_do_not_use_cli_fallback_in_strict(self) -> None:
        plan = self.plan_json(
            "--file",
            "codex-rs/http-client/src/default_client.rs",
            "--mode",
            "strict",
        )

        self.assertEqual([], plan["warnings"])
        self.assertIn("codex-http-client", plan["selected_packages"])
        self.assertIn("cli", plan["selected_surfaces"])
        commands = self.command_lines(plan)
        self.assertIn(
            [
                "just",
                "clippy-strict",
                "-p",
                "codex-http-client",
            ],
            commands,
        )

    def test_deleted_unowned_rust_path_does_not_block_current_workspace_validation(
        self,
    ) -> None:
        deleted_path = (
            self.repo_root / "codex-rs" / "deleted-package" / "src" / "lib.rs"
        )
        deleted_path.parent.mkdir(parents=True, exist_ok=True)
        deleted_path.write_text('#[cfg(feature = "removed")]\npub fn removed() {}\n')
        subprocess.run(
            ["git", "init"], cwd=self.repo_root, stdout=subprocess.DEVNULL, check=True
        )
        subprocess.run(["git", "add", "."], cwd=self.repo_root, check=True)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
            cwd=self.repo_root,
            stdout=subprocess.DEVNULL,
            check=True,
        )
        deleted_path.unlink()

        plan = self.plan_json(
            "--file", "codex-rs/deleted-package/src/lib.rs", "--mode", "standard"
        )
        commands = self.command_lines(plan)

        self.assertNotIn(["just", "fmt"], commands)

        prep_plan = self.action_json(
            "prep-plan",
            "--file",
            "codex-rs/deleted-package/src/lib.rs",
            "--mode",
            "standard",
        )
        prep_commands = self.command_lines(prep_plan)
        self.assertIn(["just", "fmt"], prep_commands)

    def test_missing_unowned_rust_path_still_fails_closed(self) -> None:
        process = self.run_planner(
            "plan",
            "--file",
            "codex-rs/missing-package/src/lib.rs",
            "--mode",
            "standard",
            "--no-receipt",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(PRODUCTION_CONFIG),
            check=False,
        )

        self.assertNotEqual(0, process.returncode)
        self.assertIn("not owned by a Cargo workspace package", process.stderr)

    def test_resource_profile_env_includes_adaptive_job_contract(self) -> None:
        plan = self.plan_json(
            "--file", "codex-rs/core/src/config/mod.rs", "--mode", "standard"
        )
        check_command = self.command_for_argv(
            plan, ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-core"]
        )
        env = check_command["env"]  # type: ignore[index]
        self.assertEqual("auto", env["CARGO_GUARD_JOBS_MODE"])
        self.assertEqual("min", env["CARGO_GUARD_JOBS_DEFAULT"])
        self.assertEqual("4", env["CARGO_GUARD_JOBS_MIN"])
        self.assertEqual("16", env["CARGO_GUARD_JOBS_MAX"])
        self.assertEqual("8192", env["CARGO_GUARD_JOBS_MEM_RESERVE_MIB"])
        self.assertEqual("0", env["CARGO_GUARD_EXPECTED_GROWTH_GIB"])
        self.assertEqual(0, check_command["fallback_expected_growth_gib"])
        self.assertEqual(0, check_command["effective_expected_growth_gib"])
        self.assertEqual("fallback:no-history", check_command["expected_growth_source"])
        self.assertIn("fingerprint", check_command)
        self.assertIn("job_contract_digest", check_command)

    def test_config_validation_rejects_stale_profile_aliases_and_bad_history_limit(
        self,
    ) -> None:
        cases = {
            "stale expected growth key": """
                [resource_profiles.check]
                reserve_free_pct = 15
                reserve_free_gib = 12
                expected_growth_gib = 6
                abort_free_pct = 6
                abort_free_gib = 6
                monitor = false
            """,
            "stale profile key": """
                [resource_profiles.check]
                reserve_free_pct = 15
                reserve_free_gib = 12
                abort_free_pct = 6
                abort_free_gib = 6
                monitor = false
                jobs_max = 4
            """,
            "zero history sample limit": """
                [defaults]
                history_sample_limit = 0
            """,
            "stale history multiplier": """
                [defaults]
                history_growth_multiplier_pct = 125
            """,
        }
        for label, extra_config in cases.items():
            with self.subTest(label=label):
                config_path = self.repo_root / f"{label.replace(' ', '-')}.toml"
                config_path.write_text(
                    textwrap.dedent(f"""
                    schema_version = 1

                    [defaults]
                    standard_mode = "standard"
                    unknown_rust_path_policy = "package-plus-cli-strict"
                    workspace_features_policy = "deny-routine-all-features"

                    [receipts]
                    dir = "receipts"

                    {extra_config}

                    [[path_rules]]
                    patterns = ["fixture.txt"]
                """).strip()
                    + "\n"
                )
                (self.repo_root / "fixture.txt").write_text("fixture\n")
                process = self.run_planner(
                    "plan",
                    "--file",
                    "fixture.txt",
                    "--repo-root",
                    str(self.repo_root),
                    "--metadata-json",
                    str(self.metadata_path),
                    "--config",
                    str(config_path),
                    "--no-receipt",
                    check=False,
                )
                self.assertNotEqual(0, process.returncode)

    def write_direct_guard_fixture(
        self, *, metrics_kind: str = "valid"
    ) -> tuple[Path, Path, Path]:
        scripts_dir = self.repo_root / "scripts"
        scripts_dir.mkdir(parents=True, exist_ok=True)
        guard_path = scripts_dir / "cargo-guard.sh"
        metrics_payload = {
            "schema_version": 1,
            "resource_profile": "__RESOURCE_PROFILE__",
            "command_fingerprint": "__FINGERPRINT__",
            "job_contract_digest": "__JOB_CONTRACT_DIGEST__",
            "cargo_subcommand": "check",
            "jobs_mode": "fixed",
            "jobs_default": "min",
            "selected_cargo_build_job_cap": 4,
            "effective_cargo_build_jobs": 4,
            "effective_cargo_build_jobs_source": "min",
            "selected_runtime_test_threads": None,
            "target_dir": "/tmp/target",
            "build_dir": "/tmp/build",
            "monitored_paths": [
                {
                    "label": "target",
                    "path": "/tmp/target",
                    "fs_id": "dev-1",
                    "cleanable": True,
                    "start_available_bytes": 20 * 1024**3,
                    "min_available_bytes": 9 * 1024**3,
                    "end_available_bytes": 12 * 1024**3,
                    "observed_growth_bytes": 11 * 1024**3,
                }
            ],
            "observed_growth_gib": 11,
            "mem_available_selection_mib": 32768,
            "disk_emergency": False,
            "status": 0,
            "telemetry_level": "__TELEMETRY_LEVEL__",
            "telemetry_schema_version": 1,
            "telemetry_log_path": "__TELEMETRY_LOG_PATH__",
            "telemetry_sample_count": 0,
            "telemetry_error_count": 0,
            "top_rustc_crates": [],
        }
        if metrics_kind == "fingerprint_mismatch":
            metrics_payload["command_fingerprint"] = "wrong"
        if metrics_kind == "profile_mismatch":
            metrics_payload["resource_profile"] = "wrong"
        if metrics_kind == "job_contract_mismatch":
            metrics_payload["job_contract_digest"] = "wrong"
        if metrics_kind == "extra_top_level":
            metrics_payload["low_disk_clamp"] = False
        if metrics_kind == "stale_field_name":
            metrics_payload["fingerprint"] = "old"
        if metrics_kind == "disk_emergency":
            metrics_payload["disk_emergency"] = True
            metrics_payload["status"] = 80
            metrics_payload["observed_growth_gib"] = 17
        if metrics_kind in {"valid", "disk_emergency"} or metrics_kind.endswith(
            "_mismatch"
        ):
            rendered_payload = repr(metrics_payload)
            metrics_code = (
                "metrics_path = os.environ.get('CARGO_GUARD_METRICS_PATH')\n"
                "if metrics_path:\n"
                f"    payload = {rendered_payload}\n"
                "    if payload['command_fingerprint'] == '__FINGERPRINT__':\n"
                "        payload['command_fingerprint'] = os.environ['CARGO_GUARD_COMMAND_FINGERPRINT']\n"
                "    if payload['resource_profile'] == '__RESOURCE_PROFILE__':\n"
                "        payload['resource_profile'] = os.environ['CARGO_GUARD_RESOURCE_PROFILE']\n"
                "    if payload['job_contract_digest'] == '__JOB_CONTRACT_DIGEST__':\n"
                "        jobs_max = int(os.environ.get('CARGO_GUARD_JOBS_MAX', '4'))\n"
                "        digest_payload = {\n"
                "            'schema': 1,\n"
                "            'resource_profile': os.environ.get('CARGO_GUARD_RESOURCE_PROFILE') or None,\n"
                "            'jobs_mode': os.environ.get('CARGO_GUARD_JOBS_MODE', 'fixed'),\n"
                "            'jobs_default': os.environ.get('CARGO_GUARD_JOBS_DEFAULT', 'min'),\n"
                "            'jobs_min': int(os.environ.get('CARGO_GUARD_JOBS_MIN', '4')),\n"
                "            'jobs_max': jobs_max,\n"
                "            'jobs_hard_max': int(os.environ.get('CARGO_GUARD_JOBS_HARD_MAX', str(jobs_max))),\n"
                "            'jobs_low_disk_max': int(os.environ.get('CARGO_GUARD_LOW_DISK_JOBS_MAX', str(jobs_max))),\n"
                "            'jobs_cpu_pct': int(os.environ.get('CARGO_GUARD_JOBS_CPU_PCT', '100')),\n"
                "            'jobs_cpu_reserve': int(os.environ.get('CARGO_GUARD_JOBS_CPU_RESERVE', '0')),\n"
                "            'jobs_mem_per_job_mib': int(os.environ.get('CARGO_GUARD_JOBS_MEM_PER_JOB_MIB', '1')),\n"
                "            'jobs_mem_reserve_mib': int(os.environ.get('CARGO_GUARD_JOBS_MEM_RESERVE_MIB', '0')),\n"
                "        }\n"
                "        payload['job_contract_digest'] = hashlib.sha256(json.dumps(digest_payload, sort_keys=True, separators=(',', ':')).encode()).hexdigest()\n"
                "    if payload['telemetry_level'] == '__TELEMETRY_LEVEL__':\n"
                "        payload['telemetry_level'] = os.environ.get('CARGO_GUARD_TELEMETRY_LEVEL', 'off')\n"
                "    if payload['telemetry_log_path'] == '__TELEMETRY_LOG_PATH__':\n"
                "        payload['telemetry_log_path'] = os.environ.get('CARGO_GUARD_TELEMETRY_PATH')\n"
                "    Path(metrics_path).write_text(json.dumps(payload, sort_keys=True) + '\\n')\n"
            )
        elif metrics_kind == "malformed":
            metrics_code = (
                "metrics_path = os.environ.get('CARGO_GUARD_METRICS_PATH')\n"
                "if metrics_path:\n"
                "    Path(metrics_path).write_text('{not json')\n"
            )
        elif metrics_kind in {"extra_top_level", "stale_field_name"}:
            rendered_payload = repr(metrics_payload)
            metrics_code = (
                "metrics_path = os.environ.get('CARGO_GUARD_METRICS_PATH')\n"
                "if metrics_path:\n"
                f"    payload = {rendered_payload}\n"
                "    if payload['command_fingerprint'] == '__FINGERPRINT__':\n"
                "        payload['command_fingerprint'] = os.environ['CARGO_GUARD_COMMAND_FINGERPRINT']\n"
                "    if payload['resource_profile'] == '__RESOURCE_PROFILE__':\n"
                "        payload['resource_profile'] = os.environ['CARGO_GUARD_RESOURCE_PROFILE']\n"
                "    if payload.get('job_contract_digest') == '__JOB_CONTRACT_DIGEST__':\n"
                "        payload['job_contract_digest'] = 'bad-digest-for-extra-field-case'\n"
                "    if payload.get('telemetry_level') == '__TELEMETRY_LEVEL__':\n"
                "        payload['telemetry_level'] = os.environ.get('CARGO_GUARD_TELEMETRY_LEVEL', 'off')\n"
                "    if payload.get('telemetry_log_path') == '__TELEMETRY_LOG_PATH__':\n"
                "        payload['telemetry_log_path'] = os.environ.get('CARGO_GUARD_TELEMETRY_PATH')\n"
                "    Path(metrics_path).write_text(json.dumps(payload, sort_keys=True) + '\\n')\n"
            )
        elif metrics_kind == "missing":
            metrics_code = ""
        else:
            raise AssertionError(f"unknown metrics kind: {metrics_kind}")
        exit_code = 80 if metrics_kind == "disk_emergency" else 0
        guard_path.write_text(
            "#!/usr/bin/env python3\n"
            "import hashlib\n"
            "import json\n"
            "import os\n"
            "import sys\n"
            "from pathlib import Path\n"
            "Path(os.environ['CARGO_VALIDATE_TEST_LOG']).write_text(' '.join(sys.argv[1:]) + '\\n')\n"
            "v8_log = os.environ.get('CARGO_VALIDATE_V8_TEST_LOG')\n"
            "if v8_log:\n"
            "    Path(v8_log).write_text('|'.join([\n"
            "        os.environ.get('RUSTY_V8_ARCHIVE', ''),\n"
            "        os.environ.get('RUSTY_V8_SRC_BINDING_PATH', ''),\n"
            "        os.environ.get('V8_FROM_SOURCE', ''),\n"
            "        os.environ.get('RUSTY_V8_MIRROR', ''),\n"
            "    ]) + '\\n')\n"
            f"{metrics_code}"
            f"raise SystemExit({exit_code})\n"
        )
        guard_path.chmod(0o755)

        config_path = self.repo_root / "direct-guard-config.toml"
        config_path.write_text(
            textwrap.dedent("""
            schema_version = 1

            [defaults]
            standard_mode = "standard"
            unknown_rust_path_policy = "disabled"
            workspace_features_policy = "deny-routine-all-features"
            history_sample_limit = 2

            [receipts]
            dir = "receipts"

            [resource_profiles.check]
            reserve_free_pct = 0
            reserve_free_gib = 5
            abort_free_pct = 0
            abort_free_gib = 5
            monitor = false

            [commands.guard-check]
            argv = ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-core"]
            profile = "check"

            [[path_rules]]
            patterns = ["fixture.txt"]
            commands = ["guard-check"]
        """).strip()
            + "\n"
        )
        (self.repo_root / "fixture.txt").write_text("fixture\n")
        command_log = self.repo_root / "guard-command-log.txt"
        return config_path, self.metadata_path, command_log

    def run_direct_guard_plan(
        self, config_path: Path, receipt_dir: Path
    ) -> dict[str, object]:
        process = self.run_planner(
            "plan",
            "--json",
            "--file",
            "fixture.txt",
            "--mode",
            "quick",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(config_path),
            "--receipt-dir",
            str(receipt_dir),
        )
        return json.loads(process.stdout)

    def run_direct_guard_verify(
        self,
        *,
        metrics_kind: str,
        receipt_dir_name: str,
        extra_args: tuple[str, ...] = (),
    ) -> tuple[subprocess.CompletedProcess[str], Path]:
        config_path, metadata_path, command_log = self.write_direct_guard_fixture(
            metrics_kind=metrics_kind
        )
        receipt_dir = self.repo_root / receipt_dir_name
        env = os.environ.copy()
        env["CARGO_VALIDATE_TEST_LOG"] = str(command_log)
        process = self.run_planner(
            "verify",
            "--file",
            "fixture.txt",
            "--mode",
            "quick",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(metadata_path),
            "--config",
            str(config_path),
            "--receipt-dir",
            str(receipt_dir),
            *extra_args,
            check=False,
            env=env,
        )
        return process, receipt_dir

    def test_history_tuning_uses_recent_matching_successes(self) -> None:
        config_path, _metadata_path, _command_log = self.write_direct_guard_fixture()
        receipt_dir = self.repo_root / "history-receipts"
        initial_plan = self.run_direct_guard_plan(config_path, receipt_dir)
        command = self.command_for_argv(
            initial_plan,
            ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-core"],
        )
        fingerprint = command["fingerprint"]
        self.assertEqual(0, command["fallback_expected_growth_gib"])
        self.assertEqual(0, command["effective_expected_growth_gib"])
        self.assertEqual("0", command["env"]["CARGO_GUARD_EXPECTED_GROWTH_GIB"])  # type: ignore[index]
        self.assertEqual("fallback:no-history", command["expected_growth_source"])
        history_path = receipt_dir / "history.jsonl"
        history_path.write_text(
            "\n".join(
                json.dumps(entry)
                for entry in [
                    {
                        "resource_profile": "check",
                        "fingerprint": fingerprint,
                        "risk_kind": "success",
                        "status": 0,
                        "disk_emergency": False,
                        "observed_growth_gib": 100,
                    },
                    {
                        "resource_profile": "other",
                        "fingerprint": fingerprint,
                        "risk_kind": "success",
                        "status": 0,
                        "disk_emergency": False,
                        "observed_growth_gib": 50,
                    },
                    {
                        "resource_profile": "check",
                        "fingerprint": fingerprint,
                        "risk_kind": "success",
                        "status": 7,
                        "disk_emergency": False,
                        "observed_growth_gib": 40,
                    },
                    {
                        "resource_profile": "check",
                        "fingerprint": fingerprint,
                        "risk_kind": "success",
                        "status": 0,
                        "disk_emergency": False,
                        "observed_growth_gib": 4,
                    },
                    {
                        "resource_profile": "check",
                        "fingerprint": fingerprint,
                        "risk_kind": "success",
                        "status": 0,
                        "disk_emergency": False,
                        "observed_growth_gib": 8,
                    },
                ]
            )
            + "\n"
        )
        tuned_plan = self.run_direct_guard_plan(config_path, receipt_dir)
        tuned_command = self.command_for_argv(
            tuned_plan,
            ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-core"],
        )
        self.assertEqual(8, tuned_command["effective_expected_growth_gib"])
        self.assertEqual("8", tuned_command["env"]["CARGO_GUARD_EXPECTED_GROWTH_GIB"])  # type: ignore[index]
        self.assertEqual(
            "history:success:max=8,samples=2", tuned_command["expected_growth_source"]
        )

    def test_history_tuning_uses_matching_disk_emergencies(self) -> None:
        config_path, _metadata_path, _command_log = self.write_direct_guard_fixture()
        receipt_dir = self.repo_root / "risk-history-receipts"
        initial_plan = self.run_direct_guard_plan(config_path, receipt_dir)
        command = self.command_for_argv(
            initial_plan,
            ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-core"],
        )
        fingerprint = command["fingerprint"]
        (receipt_dir / "history.jsonl").write_text(
            "\n".join(
                json.dumps(entry)
                for entry in [
                    {
                        "resource_profile": "check",
                        "fingerprint": fingerprint,
                        "risk_kind": "success",
                        "status": 0,
                        "disk_emergency": False,
                        "observed_growth_gib": 8,
                    },
                    {
                        "resource_profile": "check",
                        "fingerprint": fingerprint,
                        "risk_kind": "disk_emergency",
                        "status": 80,
                        "disk_emergency": True,
                        "observed_growth_gib": 17,
                    },
                    {
                        "resource_profile": "check",
                        "fingerprint": "other",
                        "risk_kind": "disk_emergency",
                        "status": 80,
                        "disk_emergency": True,
                        "observed_growth_gib": 100,
                    },
                    {
                        "resource_profile": "check",
                        "fingerprint": fingerprint,
                        "risk_kind": "success",
                        "status": 7,
                        "disk_emergency": False,
                        "observed_growth_gib": 40,
                    },
                ]
            )
            + "\n"
        )
        tuned_plan = self.run_direct_guard_plan(config_path, receipt_dir)
        tuned_command = self.command_for_argv(
            tuned_plan,
            ["./scripts/cargo-guard.sh", "cargo", "check", "-p", "codex-core"],
        )
        self.assertEqual(17, tuned_command["effective_expected_growth_gib"])
        self.assertEqual("17", tuned_command["env"]["CARGO_GUARD_EXPECTED_GROWTH_GIB"])  # type: ignore[index]
        self.assertEqual(
            "history:success:max=8,samples=1;disk_emergency:max=17,samples=1",
            tuned_command["expected_growth_source"],
        )

    def test_history_tuning_ignores_non_direct_profiled_commands(self) -> None:
        config_path = self.repo_root / "profiled-just-config.toml"
        config_path.write_text(
            textwrap.dedent("""
            schema_version = 1

            [defaults]
            standard_mode = "standard"
            unknown_rust_path_policy = "disabled"
            workspace_features_policy = "deny-routine-all-features"
            history_sample_limit = 2

            [receipts]
            dir = "receipts"

            [resource_profiles.check]
            reserve_free_pct = 0
            reserve_free_gib = 5
            abort_free_pct = 0
            abort_free_gib = 5
            monitor = false

            [commands.just-check]
            argv = ["just", "thing"]
            profile = "check"

            [[path_rules]]
            patterns = ["fixture.txt"]
            commands = ["just-check"]
        """).strip()
            + "\n"
        )
        metadata_path = self.repo_root / "empty-metadata.json"
        metadata_path.write_text(json.dumps({"packages": []}))
        (self.repo_root / "fixture.txt").write_text("fixture\n")
        receipt_dir = self.repo_root / "profiled-just-receipts"
        receipt_dir.mkdir()
        payload = json.dumps(
            {"resource_profile": "check", "argv": ["just", "thing"]},
            sort_keys=True,
            separators=(",", ":"),
        )
        fingerprint = hashlib.sha256(payload.encode("utf-8")).hexdigest()
        (receipt_dir / "history.jsonl").write_text(
            json.dumps(
                {
                    "resource_profile": "check",
                    "fingerprint": fingerprint,
                    "risk_kind": "success",
                    "status": 0,
                    "disk_emergency": False,
                    "observed_growth_gib": 100,
                }
            )
            + "\n"
        )
        process = self.run_planner(
            "plan",
            "--json",
            "--file",
            "fixture.txt",
            "--mode",
            "quick",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(metadata_path),
            "--config",
            str(config_path),
            "--receipt-dir",
            str(receipt_dir),
        )
        plan = json.loads(process.stdout)
        command = self.command_for_argv(plan, ["just", "thing"])
        self.assertNotIn("fallback_expected_growth_gib", command)
        self.assertNotIn("effective_expected_growth_gib", command)
        self.assertNotIn("expected_growth_source", command)
        self.assertNotIn("CARGO_GUARD_EXPECTED_GROWTH_GIB", command["env"])  # type: ignore[index]

    def test_verify_passes_telemetry_to_guarded_just_recipe(self) -> None:
        config_path = self.repo_root / "guarded-just-config.toml"
        config_path.write_text(
            textwrap.dedent("""
            schema_version = 1

            [defaults]
            standard_mode = "standard"
            unknown_rust_path_policy = "disabled"
            workspace_features_policy = "deny-routine-all-features"
            history_sample_limit = 2

            [receipts]
            dir = "receipts"

            [resource_profiles.build]
            reserve_free_pct = 0
            reserve_free_gib = 5
            abort_free_pct = 0
            abort_free_gib = 5
            monitor = false

            [commands.guarded-just]
            argv = ["just", "strict-codex-bin"]
            profile = "build"

            [[path_rules]]
            patterns = ["fixture.txt"]
            commands = ["guarded-just"]
        """).strip()
            + "\n"
        )
        (self.repo_root / "fixture.txt").write_text("fixture\n")
        fake_bin = self.repo_root / "fake-bin"
        fake_bin.mkdir()
        fake_just = fake_bin / "just"
        command_log = self.repo_root / "just-command-log.json"
        fake_just.write_text(
            "#!/usr/bin/env python3\n"
            "import json\n"
            "import os\n"
            "import sys\n"
            "from pathlib import Path\n"
            "telemetry_path = os.environ.get('CARGO_GUARD_TELEMETRY_PATH')\n"
            "Path(os.environ['CARGO_VALIDATE_TEST_LOG']).write_text(json.dumps({\n"
            "    'argv': sys.argv[1:],\n"
            "    'telemetry_level': os.environ.get('CARGO_GUARD_TELEMETRY_LEVEL'),\n"
            "    'telemetry_path': telemetry_path,\n"
            "    'metrics_path': os.environ.get('CARGO_GUARD_METRICS_PATH'),\n"
            "}) + '\\n')\n"
            "if telemetry_path:\n"
            "    path = Path(telemetry_path)\n"
            "    path.parent.mkdir(parents=True, exist_ok=True)\n"
            "    path.write_text('schema_version\\trow_type\\n1\\taggregate\\n')\n"
        )
        fake_just.chmod(0o755)
        receipt_dir = self.repo_root / "guarded-just-receipts"
        env = os.environ.copy()
        env["CARGO_VALIDATE_TEST_LOG"] = str(command_log)
        env["PATH"] = f"{fake_bin}{os.pathsep}{env['PATH']}"

        process = self.run_planner(
            "verify",
            "--file",
            "fixture.txt",
            "--mode",
            "quick",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(config_path),
            "--receipt-dir",
            str(receipt_dir),
            check=False,
            env=env,
        )

        self.assertEqual(0, process.returncode, process.stderr)
        command_log_payload = json.loads(command_log.read_text())
        self.assertEqual(["strict-codex-bin"], command_log_payload["argv"])
        self.assertEqual("full", command_log_payload["telemetry_level"])
        self.assertIsNone(command_log_payload["metrics_path"])
        run_entry = json.loads(
            (receipt_dir / "last-run.jsonl").read_text().splitlines()[0]
        )
        self.assertIn("telemetry_log_path", run_entry)
        telemetry_path = receipt_dir / run_entry["telemetry_log_path"]
        self.assertEqual(str(telemetry_path), command_log_payload["telemetry_path"])
        self.assertTrue(telemetry_path.exists())
        self.assertNotIn("guard_metrics", run_entry)

    def test_verify_passes_fingerprint_reads_metrics_and_appends_history(self) -> None:
        process, receipt_dir = self.run_direct_guard_verify(
            metrics_kind="valid", receipt_dir_name="direct-receipts"
        )
        self.assertEqual(0, process.returncode, process.stderr)
        run_entry = json.loads(
            (receipt_dir / "last-run.jsonl").read_text().splitlines()[0]
        )
        self.assertEqual(
            run_entry["fingerprint"], run_entry["guard_metrics"]["command_fingerprint"]
        )
        self.assertEqual(
            run_entry["job_contract_digest"],
            run_entry["guard_metrics"]["job_contract_digest"],
        )
        self.assertEqual("full", run_entry["guard_metrics"]["telemetry_level"])
        self.assertIn("telemetry_log_path", run_entry)
        self.assertEqual(
            str(receipt_dir / run_entry["telemetry_log_path"]),
            run_entry["guard_metrics"]["telemetry_log_path"],
        )
        history_entry = json.loads(
            (receipt_dir / "history.jsonl").read_text().splitlines()[0]
        )
        self.assertEqual(run_entry["fingerprint"], history_entry["fingerprint"])
        self.assertEqual(
            run_entry["job_contract_digest"], history_entry["job_contract_digest"]
        )
        self.assertEqual("success", history_entry["risk_kind"])
        self.assertFalse(history_entry["disk_emergency"])
        self.assertEqual(11, history_entry["observed_growth_gib"])
        self.assertEqual("min", history_entry["jobs_default"])
        self.assertEqual("min", history_entry["selected_jobs_source"])

    def test_verify_appends_disk_emergency_history_when_metrics_are_valid(self) -> None:
        process, receipt_dir = self.run_direct_guard_verify(
            metrics_kind="disk_emergency", receipt_dir_name="disk-emergency-receipts"
        )
        self.assertEqual(80, process.returncode, process.stderr)
        run_entry = json.loads(
            (receipt_dir / "last-run.jsonl").read_text().splitlines()[0]
        )
        self.assertTrue(run_entry["guard_metrics"]["disk_emergency"])
        history_entry = json.loads(
            (receipt_dir / "history.jsonl").read_text().splitlines()[0]
        )
        self.assertEqual(run_entry["fingerprint"], history_entry["fingerprint"])
        self.assertEqual(
            run_entry["job_contract_digest"], history_entry["job_contract_digest"]
        )
        self.assertEqual("disk_emergency", history_entry["risk_kind"])
        self.assertTrue(history_entry["disk_emergency"])
        self.assertEqual(80, history_entry["status"])
        self.assertEqual(17, history_entry["observed_growth_gib"])

    def test_verify_resume_ignores_expected_growth_only_changes(self) -> None:
        first, receipt_dir = self.run_direct_guard_verify(
            metrics_kind="valid", receipt_dir_name="expected-growth-resume-receipts"
        )
        self.assertEqual(0, first.returncode, first.stderr)

        second = self.run_planner(
            "verify",
            "--file",
            "fixture.txt",
            "--mode",
            "quick",
            "--resume",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(self.repo_root / "direct-guard-config.toml"),
            "--receipt-dir",
            str(receipt_dir),
            check=False,
            env={
                **os.environ,
                "CARGO_VALIDATE_TEST_LOG": str(
                    self.repo_root / "guard-command-log.txt"
                ),
            },
        )
        self.assertEqual(0, second.returncode, second.stderr)
        self.assertEqual(
            "cargo check -p codex-core\n",
            (self.repo_root / "guard-command-log.txt").read_text(),
        )
        run_entry = json.loads(
            (receipt_dir / "last-run.jsonl").read_text().splitlines()[0]
        )
        self.assertEqual("skipped", run_entry["coverage"])
        self.assertEqual("resume", run_entry["coverage_source"])
        summary = json.loads((receipt_dir / "last-run-summary.json").read_text())
        self.assertEqual("full", summary["coverage"])
        self.assertEqual("full", summary["telemetry_level"])

    def test_verify_resume_reruns_after_telemetry_level_changes(self) -> None:
        first, receipt_dir = self.run_direct_guard_verify(
            metrics_kind="valid", receipt_dir_name="telemetry-level-resume-receipts"
        )
        self.assertEqual(0, first.returncode, first.stderr)

        second = self.run_planner(
            "verify",
            "--file",
            "fixture.txt",
            "--mode",
            "quick",
            "--resume",
            "--telemetry-level",
            "summary",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(self.metadata_path),
            "--config",
            str(self.repo_root / "direct-guard-config.toml"),
            "--receipt-dir",
            str(receipt_dir),
            check=False,
            env={
                **os.environ,
                "CARGO_VALIDATE_TEST_LOG": str(
                    self.repo_root / "guard-command-log.txt"
                ),
            },
        )
        self.assertEqual(0, second.returncode, second.stderr)
        run_entry = json.loads(
            (receipt_dir / "last-run.jsonl").read_text().splitlines()[0]
        )
        self.assertEqual("executed", run_entry["coverage"])
        self.assertEqual("summary", run_entry["guard_metrics"]["telemetry_level"])
        summary = json.loads((receipt_dir / "last-run-summary.json").read_text())
        self.assertEqual("summary", summary["telemetry_level"])

    def test_verify_fails_loud_when_guard_metrics_are_missing(self) -> None:
        process, receipt_dir = self.run_direct_guard_verify(
            metrics_kind="missing", receipt_dir_name="missing-metrics-receipts"
        )
        self.assertEqual(2, process.returncode)
        self.assertIn("guard metrics were not written", process.stderr)
        run_entry = json.loads(
            (receipt_dir / "last-run.jsonl").read_text().splitlines()[0]
        )
        self.assertIn("guard_metrics_error", run_entry)
        self.assertIn(
            "guard metrics were not written", run_entry["guard_metrics_error"]
        )
        self.assertFalse((receipt_dir / "history.jsonl").exists())

    def test_verify_records_guard_metrics_error_for_malformed_and_mismatch_cases(
        self,
    ) -> None:
        cases = {
            "malformed": "guard metrics are malformed JSON",
            "fingerprint_mismatch": "guard metrics fingerprint mismatch",
            "profile_mismatch": "guard metrics resource_profile mismatch",
            "job_contract_mismatch": "guard metrics job_contract_digest mismatch",
            "extra_top_level": "guard metrics contain unknown top-level keys: low_disk_clamp",
            "stale_field_name": "guard metrics contain unknown top-level keys: fingerprint",
        }
        for metrics_kind, expected_error in cases.items():
            with self.subTest(metrics_kind=metrics_kind):
                process, receipt_dir = self.run_direct_guard_verify(
                    metrics_kind=metrics_kind,
                    receipt_dir_name=f"{metrics_kind}-receipts",
                )
                self.assertEqual(2, process.returncode)
                self.assertIn(expected_error, process.stderr)
                run_entry = json.loads(
                    (receipt_dir / "last-run.jsonl").read_text().splitlines()[0]
                )
                self.assertIn(expected_error, run_entry["guard_metrics_error"])
                self.assertFalse((receipt_dir / "history.jsonl").exists())

    def write_verify_fixture(self) -> tuple[Path, Path, Path]:
        command_log = self.repo_root / "command-log.txt"
        stub_path = self.repo_root / "stub_command.py"
        stub_path.write_text(
            "import os\n"
            "import sys\n"
            "from pathlib import Path\n"
            'Path(os.environ["CARGO_VALIDATE_TEST_LOG"]).open("a").write(sys.argv[1] + "\\n")\n'
            'if sys.argv[1].startswith("fail"):\n'
            "    raise SystemExit(7)\n"
        )
        config_path = self.repo_root / "verify-config.toml"
        config_path.write_text(
            textwrap.dedent(f"""
            # Merge-safety anchor: test fixture for cargo-validate verify behavior.
            schema_version = 1

            [defaults]
            standard_mode = "standard"
            unknown_rust_path_policy = "package-plus-cli-strict"
            workspace_features_policy = "deny-routine-all-features"

            [receipts]
            dir = "receipts"

            [commands.ok-one]
            argv = ["{sys.executable}", "{stub_path}", "ok-one"]

            [commands.fail-two]
            argv = ["{sys.executable}", "{stub_path}", "fail-two"]

            [commands.ok-three]
            argv = ["{sys.executable}", "{stub_path}", "ok-three"]

            [[path_rules]]
            patterns = ["fixture.txt"]
            commands = ["ok-one", "fail-two", "ok-three"]
        """).strip()
            + "\n"
        )
        metadata_path = self.repo_root / "empty-metadata.json"
        metadata_path.write_text(json.dumps({"packages": []}))
        (self.repo_root / "fixture.txt").write_text("fixture\n")
        return config_path, metadata_path, command_log

    def run_verify_fixture(
        self, *extra_args: str
    ) -> tuple[subprocess.CompletedProcess[str], Path]:
        config_path, metadata_path, command_log = self.write_verify_fixture()
        receipt_dir = self.repo_root / "verify-receipts"
        env = os.environ.copy()
        env["CARGO_VALIDATE_TEST_LOG"] = str(command_log)
        process = self.run_planner(
            "verify",
            "--file",
            "fixture.txt",
            "--mode",
            "standard",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(metadata_path),
            "--config",
            str(config_path),
            "--receipt-dir",
            str(receipt_dir),
            *extra_args,
            check=False,
            env=env,
        )
        return process, receipt_dir

    def write_prep_fixture(self) -> tuple[Path, Path, Path]:
        command_log = self.repo_root / "prep-command-log.txt"
        stub_path = self.repo_root / "prep_stub_command.py"
        stub_path.write_text(
            "import os\n"
            "import sys\n"
            "from pathlib import Path\n"
            'Path(os.environ["CARGO_VALIDATE_TEST_LOG"]).open("a").write(sys.argv[1] + "\\n")\n'
        )
        config_path = self.repo_root / "prep-config.toml"
        config_path.write_text(
            textwrap.dedent(f"""
            # Merge-safety anchor: test fixture for cargo-validate prep receipt behavior.
            schema_version = 1

            [defaults]
            standard_mode = "standard"
            unknown_rust_path_policy = "package-plus-cli-strict"
            workspace_features_policy = "deny-routine-all-features"

            [receipts]
            dir = "receipts"

            [commands.ok-prep]
            argv = ["{sys.executable}", "{stub_path}", "ok-prep"]

            [commands.validation-only]
            argv = ["{sys.executable}", "{stub_path}", "validation-only"]

            [[path_rules]]
            patterns = ["fixture.txt"]
            prep_commands = ["ok-prep"]
            commands = ["validation-only"]
        """).strip()
            + "\n"
        )
        metadata_path = self.repo_root / "empty-metadata.json"
        metadata_path.write_text(json.dumps({"packages": []}))
        (self.repo_root / "fixture.txt").write_text("fixture\n")
        return config_path, metadata_path, command_log

    def test_prep_executes_only_prep_commands_and_marks_receipt_stage(self) -> None:
        config_path, metadata_path, command_log = self.write_prep_fixture()
        receipt_dir = self.repo_root / "prep-receipts"
        env = os.environ.copy()
        env["CARGO_VALIDATE_TEST_LOG"] = str(command_log)
        process = self.run_planner(
            "prep",
            "--file",
            "fixture.txt",
            "--mode",
            "standard",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(metadata_path),
            "--config",
            str(config_path),
            "--receipt-dir",
            str(receipt_dir),
            check=False,
            env=env,
        )

        self.assertEqual(0, process.returncode, process.stderr)
        self.assertEqual(["ok-prep"], command_log.read_text().splitlines())
        summary = json.loads((receipt_dir / "last-run-summary.json").read_text())
        self.assertEqual("prep", summary["action"])
        self.assertEqual("prep", summary["stage"])
        run_entries = [
            json.loads(line)
            for line in (receipt_dir / "last-run.jsonl").read_text().splitlines()
        ]
        self.assertEqual(["ok-prep"], [entry["argv"][-1] for entry in run_entries])
        self.assertEqual(["prep"], [entry["action"] for entry in run_entries])
        self.assertEqual(["prep"], [entry["stage"] for entry in run_entries])

    def write_shared_prep_verify_fixture(self) -> tuple[Path, Path, Path]:
        command_log = self.repo_root / "shared-prep-verify-command-log.txt"
        stub_path = self.repo_root / "shared_prep_verify_stub.py"
        stub_path.write_text(
            "import os\n"
            "import sys\n"
            "from pathlib import Path\n"
            'Path(os.environ["CARGO_VALIDATE_TEST_LOG"]).open("a").write(sys.argv[1] + "\\n")\n'
        )
        config_path = self.repo_root / "shared-prep-verify-config.toml"
        config_path.write_text(
            textwrap.dedent(f"""
            # Merge-safety anchor: test fixture for prep-vs-validation receipt identity.
            schema_version = 1

            [defaults]
            standard_mode = "standard"
            unknown_rust_path_policy = "package-plus-cli-strict"
            workspace_features_policy = "deny-routine-all-features"

            [receipts]
            dir = "receipts"

            [commands.shared]
            argv = ["{sys.executable}", "{stub_path}", "shared"]

            [[path_rules]]
            patterns = ["fixture.txt"]
            prep_commands = ["shared"]
            commands = ["shared"]
        """).strip()
            + "\n"
        )
        metadata_path = self.repo_root / "empty-shared-prep-verify-metadata.json"
        metadata_path.write_text(json.dumps({"packages": []}))
        (self.repo_root / "fixture.txt").write_text("fixture\n")
        return config_path, metadata_path, command_log

    def test_verify_resume_does_not_reuse_prep_receipts(self) -> None:
        config_path, metadata_path, command_log = (
            self.write_shared_prep_verify_fixture()
        )
        receipt_dir = self.repo_root / "shared-prep-verify-receipts"
        env = os.environ.copy()
        env["CARGO_VALIDATE_TEST_LOG"] = str(command_log)
        prep = self.run_planner(
            "prep",
            "--file",
            "fixture.txt",
            "--mode",
            "standard",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(metadata_path),
            "--config",
            str(config_path),
            "--receipt-dir",
            str(receipt_dir),
            check=False,
            env=env,
        )
        self.assertEqual(0, prep.returncode, prep.stderr)

        verify = self.run_planner(
            "verify",
            "--file",
            "fixture.txt",
            "--mode",
            "standard",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(metadata_path),
            "--config",
            str(config_path),
            "--receipt-dir",
            str(receipt_dir),
            "--resume",
            "--explain-skip",
            check=False,
            env=env,
        )

        self.assertEqual(0, verify.returncode, verify.stderr)
        self.assertEqual(["shared", "shared"], command_log.read_text().splitlines())
        self.assertNotIn("[cargo-validate][skip]", verify.stdout)
        summary = json.loads((receipt_dir / "last-run-summary.json").read_text())
        self.assertEqual("verify", summary["action"])
        self.assertEqual("validation", summary["stage"])
        run_entries = [
            json.loads(line)
            for line in (receipt_dir / "last-run.jsonl").read_text().splitlines()
        ]
        self.assertEqual(["executed"], [entry["coverage"] for entry in run_entries])
        self.assertEqual(["verify"], [entry["action"] for entry in run_entries])
        self.assertEqual(["validation"], [entry["stage"] for entry in run_entries])

    def test_verify_fail_fast_stops_at_first_failure_and_writes_run_receipt(
        self,
    ) -> None:
        process, receipt_dir = self.run_verify_fixture("--fail-fast")
        self.assertEqual(7, process.returncode)
        command_log = (self.repo_root / "command-log.txt").read_text().splitlines()
        self.assertEqual(["ok-one", "fail-two"], command_log)
        run_entries = [
            json.loads(line)
            for line in (receipt_dir / "last-run.jsonl").read_text().splitlines()
        ]
        self.assertEqual([0, 7, None], [entry["status"] for entry in run_entries])
        self.assertEqual(
            ["executed", "executed", "skipped"],
            [entry["coverage"] for entry in run_entries],
        )
        summary = json.loads((receipt_dir / "last-run-summary.json").read_text())
        self.assertEqual("partial", summary["coverage"])
        self.assertEqual(7, summary["status"])

    def test_verify_continues_after_failure_by_default(self) -> None:
        process, receipt_dir = self.run_verify_fixture()
        self.assertEqual(7, process.returncode)
        command_log = (self.repo_root / "command-log.txt").read_text().splitlines()
        self.assertEqual(["ok-one", "fail-two", "ok-three"], command_log)
        run_entries = [
            json.loads(line)
            for line in (receipt_dir / "last-run.jsonl").read_text().splitlines()
        ]
        self.assertEqual([0, 7, 0], [entry["status"] for entry in run_entries])

    def test_verify_records_stdout_and_stderr_logs_for_each_executed_command(
        self,
    ) -> None:
        stub_path = self.repo_root / "output_stub_command.py"
        stub_path.write_text(
            "import sys\n"
            "label = sys.argv[1]\n"
            "print(f'stdout:{label}')\n"
            "print(f'stderr:{label}', file=sys.stderr)\n"
        )
        config_path = self.repo_root / "output-log-config.toml"
        config_path.write_text(
            textwrap.dedent(f"""
            # Merge-safety anchor: test fixture for cargo-validate command output logs.
            schema_version = 1

            [defaults]
            standard_mode = "standard"
            unknown_rust_path_policy = "package-plus-cli-strict"
            workspace_features_policy = "deny-routine-all-features"

            [receipts]
            dir = "receipts"

            [commands.ok-one]
            argv = ["{sys.executable}", "{stub_path}", "ok-one"]

            [commands.ok-two]
            argv = ["{sys.executable}", "{stub_path}", "ok-two"]

            [[path_rules]]
            patterns = ["fixture.txt"]
            commands = ["ok-one", "ok-two"]
        """).strip()
            + "\n"
        )
        metadata_path = self.repo_root / "empty-output-log-metadata.json"
        metadata_path.write_text(json.dumps({"packages": []}))
        (self.repo_root / "fixture.txt").write_text("fixture\n")
        receipt_dir = self.repo_root / "output-log-receipts"

        process = self.run_planner(
            "verify",
            "--file",
            "fixture.txt",
            "--mode",
            "standard",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(metadata_path),
            "--config",
            str(config_path),
            "--receipt-dir",
            str(receipt_dir),
            check=False,
        )

        self.assertEqual(0, process.returncode, process.stderr)
        self.assertIn("stdout:ok-one", process.stdout)
        self.assertIn("stdout:ok-two", process.stdout)
        self.assertIn("stderr:ok-one", process.stderr)
        self.assertIn("stderr:ok-two", process.stderr)
        summary = json.loads((receipt_dir / "last-run-summary.json").read_text())
        self.assertTrue(summary["command_log_dir"].startswith("command-logs/"))
        run_entries = [
            json.loads(line)
            for line in (receipt_dir / "last-run.jsonl").read_text().splitlines()
        ]
        self.assertEqual(2, len(run_entries))
        for label, entry in zip(("ok-one", "ok-two"), run_entries):
            stdout_log_path = receipt_dir / entry["stdout_log_path"]
            stderr_log_path = receipt_dir / entry["stderr_log_path"]
            self.assertEqual(f"stdout:{label}\n", stdout_log_path.read_text())
            self.assertEqual(f"stderr:{label}\n", stderr_log_path.read_text())

    def test_verify_streams_sparse_stdout_before_command_exits(self) -> None:
        stub_path = self.repo_root / "sparse_output_stub.py"
        stub_path.write_text(
            "import time\n"
            "print('stdout:first', flush=True)\n"
            "time.sleep(5)\n"
            "print('stdout:done', flush=True)\n"
        )
        config_path = self.repo_root / "sparse-output-config.toml"
        config_path.write_text(
            textwrap.dedent(f"""
            # Merge-safety anchor: test fixture for live cargo-validate output teeing.
            schema_version = 1

            [defaults]
            standard_mode = "standard"
            unknown_rust_path_policy = "package-plus-cli-strict"
            workspace_features_policy = "deny-routine-all-features"

            [receipts]
            dir = "receipts"

            [commands.sparse-output]
            argv = ["{sys.executable}", "{stub_path}"]

            [[path_rules]]
            patterns = ["fixture.txt"]
            commands = ["sparse-output"]
        """).strip()
            + "\n"
        )
        metadata_path = self.repo_root / "empty-sparse-output-metadata.json"
        metadata_path.write_text(json.dumps({"packages": []}))
        (self.repo_root / "fixture.txt").write_text("fixture\n")
        receipt_dir = self.repo_root / "sparse-output-receipts"

        process = subprocess.Popen(
            [
                sys.executable,
                str(PLANNER),
                "verify",
                "--file",
                "fixture.txt",
                "--mode",
                "standard",
                "--repo-root",
                str(self.repo_root),
                "--metadata-json",
                str(metadata_path),
                "--config",
                str(config_path),
                "--receipt-dir",
                str(receipt_dir),
            ],
            cwd=REPO_ROOT,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.assertIsNotNone(process.stdout)
        stdout_queue: queue.Queue[str] = queue.Queue()

        def read_stdout() -> None:
            assert process.stdout is not None
            for line in process.stdout:
                stdout_queue.put(line)

        stdout_thread = threading.Thread(target=read_stdout, daemon=True)
        stdout_thread.start()
        seen_lines: list[str] = []
        try:
            deadline = time.monotonic() + 2
            saw_first_output = False
            while time.monotonic() < deadline:
                timeout = max(0.01, deadline - time.monotonic())
                try:
                    line = stdout_queue.get(timeout=timeout)
                except queue.Empty:
                    break
                seen_lines.append(line)
                if "stdout:first" in line:
                    saw_first_output = True
                    break
            self.assertTrue(saw_first_output, seen_lines)
            self.assertIsNone(process.poll(), "sparse output arrived only after exit")
            return_code = process.wait(timeout=10)
            stderr_output = process.stderr.read() if process.stderr is not None else ""
            self.assertEqual(0, return_code, stderr_output)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
            stdout_thread.join(timeout=1)
            if process.stdout is not None:
                process.stdout.close()
            if process.stderr is not None:
                process.stderr.close()

    def test_output_log_write_failure_keeps_draining_child_output(self) -> None:
        planner = load_planner_module()
        stub_path = self.repo_root / "drain_after_log_failure_stub.py"
        completion_marker = self.repo_root / "drain-completion.txt"
        log_failure_marker = self.repo_root / "log-failure.txt"
        stub_path.write_text(
            "import os\n"
            "import sys\n"
            "import time\n"
            "from pathlib import Path\n"
            "marker = Path(sys.argv[1])\n"
            "log_failure = Path(sys.argv[2])\n"
            "stdout_fd = sys.stdout.fileno()\n"
            "os.set_blocking(stdout_fd, False)\n"
            "print('trigger-log-failure', flush=True)\n"
            "deadline = time.monotonic() + 2\n"
            "while not log_failure.exists():\n"
            "    if time.monotonic() >= deadline:\n"
            "        marker.write_text('log-failure-not-observed')\n"
            "        raise SystemExit(8)\n"
            "    time.sleep(0.001)\n"
            "remaining = 2 * 1024 * 1024\n"
            "blocked_since = None\n"
            "while remaining:\n"
            "    try:\n"
            "        written = os.write(stdout_fd, b'x' * min(4096, remaining))\n"
            "        remaining -= written\n"
            "        blocked_since = None\n"
            "    except BlockingIOError:\n"
            "        if blocked_since is None:\n"
            "            blocked_since = time.monotonic()\n"
            "        elif time.monotonic() - blocked_since >= 0.5:\n"
            "            marker.write_text('blocked')\n"
            "            raise SystemExit(9)\n"
            "    time.sleep(0.001)\n"
            "marker.write_text('drained')\n"
        )
        stdout_log_path = self.repo_root / "stdout.log"
        stderr_log_path = self.repo_root / "stderr.log"
        original_open = Path.open

        class FailingLogFile:
            def __enter__(self) -> "FailingLogFile":
                return self

            def __exit__(self, *args: object) -> None:
                return None

            def write(self, _chunk: bytes) -> int:
                log_failure_marker.write_text("failed")
                raise OSError("forced log write failure")

            def flush(self) -> None:
                return None

        class DiscardingBinaryStream:
            def write(self, chunk: bytes) -> int:
                return len(chunk)

            def flush(self) -> None:
                return None

        class ConsoleStream:
            def __init__(self) -> None:
                self.buffer = DiscardingBinaryStream()

            def write(self, text: str) -> int:
                return len(text)

            def flush(self) -> None:
                return None

        def fake_open(path: Path, *args: object, **kwargs: object) -> object:
            if path == stdout_log_path:
                return FailingLogFile()
            return original_open(path, *args, **kwargs)

        started_at = time.monotonic()
        with (
            mock.patch.object(Path, "open", fake_open),
            mock.patch.object(planner.sys, "stdout", ConsoleStream()),
            self.assertRaises(planner.PlannerError) as context,
        ):
            planner.run_command_with_output_logs(
                [
                    sys.executable,
                    str(stub_path),
                    str(completion_marker),
                    str(log_failure_marker),
                ],
                cwd=self.repo_root,
                env=os.environ.copy(),
                stdout_log_path=stdout_log_path,
                stderr_log_path=stderr_log_path,
            )
        elapsed_seconds = time.monotonic() - started_at

        self.assertLess(elapsed_seconds, 2)
        self.assertIn(
            "failed to record command output: stdout log: forced log write failure",
            str(context.exception),
        )
        self.assertEqual("drained", completion_marker.read_text())

    def test_output_log_short_writes_are_completed(self) -> None:
        planner = load_planner_module()
        stub_path = self.repo_root / "short_write_stub.py"
        stub_path.write_text(
            "import sys\nsys.stdout.write('abcdef')\nsys.stdout.flush()\n"
        )
        stdout_log_path = self.repo_root / "stdout.log"
        stderr_log_path = self.repo_root / "stderr.log"
        original_open = Path.open

        class ShortWritingFile:
            def __init__(self, log_file: object) -> None:
                self.log_file = log_file

            def __enter__(self) -> "ShortWritingFile":
                self.log_file.__enter__()
                return self

            def __exit__(self, *args: object) -> object:
                return self.log_file.__exit__(*args)

            def write(self, chunk: bytes) -> int:
                return self.log_file.write(chunk[:1])

            def flush(self) -> None:
                self.log_file.flush()

        def fake_open(path: Path, *args: object, **kwargs: object) -> object:
            if path == stdout_log_path:
                return ShortWritingFile(original_open(path, *args, **kwargs))
            return original_open(path, *args, **kwargs)

        with mock.patch.object(Path, "open", fake_open):
            return_code = planner.run_command_with_output_logs(
                [sys.executable, str(stub_path)],
                cwd=self.repo_root,
                env=os.environ.copy(),
                stdout_log_path=stdout_log_path,
                stderr_log_path=stderr_log_path,
            )

        self.assertEqual(0, return_code)
        self.assertEqual("abcdef", stdout_log_path.read_text())

    def test_output_log_parent_creation_failure_is_planner_error(self) -> None:
        planner = load_planner_module()
        marker_path = self.repo_root / "command-ran.txt"
        stub_path = self.repo_root / "mkdir_failure_stub.py"
        stub_path.write_text(
            "import sys\n"
            "from pathlib import Path\n"
            "Path(sys.argv[1]).write_text('ran')\n"
        )
        conflicting_parent = self.repo_root / "stdout-parent"
        conflicting_parent.write_text("not a directory\n")

        with self.assertRaises(planner.PlannerError) as context:
            planner.run_command_with_output_logs(
                [sys.executable, str(stub_path), str(marker_path)],
                cwd=self.repo_root,
                env=os.environ.copy(),
                stdout_log_path=conflicting_parent / "stdout.log",
                stderr_log_path=self.repo_root / "stderr.log",
            )

        self.assertIn("failed to open command output logs", str(context.exception))
        self.assertFalse(marker_path.exists())

    def test_verify_records_receipt_when_output_log_fails_after_command_runs(
        self,
    ) -> None:
        planner = load_planner_module()
        config_path, metadata_path, command_log = self.write_direct_guard_fixture(
            metrics_kind="valid"
        )
        guard_path = self.repo_root / "scripts" / "cargo-guard.sh"
        guard_path.write_text(
            guard_path.read_text().replace(
                "from pathlib import Path\n",
                "from pathlib import Path\nprint('guard-output', flush=True)\n",
            )
        )
        receipt_dir = self.repo_root / "output-log-failure-receipts"
        config = planner.load_config(config_path)
        packages = planner.load_metadata(self.repo_root, metadata_path)
        plan = planner.build_plan(
            action="verify",
            stage="validation",
            mode="quick",
            files=["fixture.txt"],
            explicit_surfaces=[],
            repo_root=self.repo_root,
            config=config,
            packages=packages,
            receipt_dir=receipt_dir,
            telemetry_level="full",
        )
        original_open = Path.open

        class FailingLogFile:
            def __enter__(self) -> "FailingLogFile":
                return self

            def __exit__(self, *args: object) -> None:
                return None

            def write(self, _chunk: bytes) -> int:
                raise OSError("forced log write failure")

            def flush(self) -> None:
                return None

        class DiscardingBinaryStream:
            def write(self, chunk: bytes) -> int:
                return len(chunk)

            def flush(self) -> None:
                return None

        class ConsoleStream:
            def __init__(self) -> None:
                self.buffer = DiscardingBinaryStream()

            def write(self, text: str) -> int:
                return len(text)

            def flush(self) -> None:
                return None

        def fake_open(path: Path, *args: object, **kwargs: object) -> object:
            mode = str(args[0] if args else kwargs.get("mode", "r"))
            if path.name.endswith(".stdout.log") and "w" in mode and "b" in mode:
                return FailingLogFile()
            return original_open(path, *args, **kwargs)

        with (
            mock.patch.object(Path, "open", fake_open),
            mock.patch.object(planner.sys, "stdout", ConsoleStream()),
            mock.patch.dict(os.environ, {"CARGO_VALIDATE_TEST_LOG": str(command_log)}),
        ):
            status = planner.verify_plan(
                plan,
                self.repo_root,
                keep_going=False,
            )

        self.assertEqual(2, status)
        self.assertEqual(
            "cargo check -p codex-core\n",
            command_log.read_text(),
        )
        run_entries = [
            json.loads(line)
            for line in (receipt_dir / "last-run.jsonl").read_text().splitlines()
        ]
        self.assertEqual(1, len(run_entries))
        run_entry = run_entries[0]
        self.assertEqual("executed", run_entry["coverage"])
        self.assertEqual(0, run_entry["status"])
        self.assertIn("stdout_log_path", run_entry)
        self.assertIn("stderr_log_path", run_entry)
        self.assertIn(
            "stdout log: forced log write failure",
            run_entry["output_log_error"],
        )
        self.assertIn("guard_metrics", run_entry)
        self.assertFalse(list(receipt_dir.glob(".guard-metrics-*.json")))
        self.assertFalse((receipt_dir / "history.jsonl").exists())
        summary = json.loads((receipt_dir / "last-run-summary.json").read_text())
        self.assertEqual(2, summary["status"])
        self.assertEqual("partial", summary["coverage"])
        self.assertEqual(1, summary["executed_count"])
        self.assertEqual(0, summary["covered_count"])
        self.assertEqual(1, len(summary["failed_commands"]))
        self.assertEqual(1, len(summary["output_log_failures"]))
        self.assertIn(
            "stdout log: forced log write failure",
            summary["output_log_failures"][0]["error"],
        )

    @unittest.skipUnless(os.name == "posix", "inherited pipe behavior is POSIX-only")
    def test_wrapper_exit_does_not_wait_for_descendant_inherited_pipes(self) -> None:
        planner = load_planner_module()
        descendant_path = self.repo_root / "sleeping_descendant.py"
        descendant_path.write_text("import time\ntime.sleep(30)\n")
        descendant_pid_path = self.repo_root / "descendant.pid"
        wrapper_path = self.repo_root / "descendant_wrapper.py"
        wrapper_path.write_text(
            "import subprocess\n"
            "import sys\n"
            "from pathlib import Path\n"
            f"descendant = {str(descendant_path)!r}\n"
            f"pid_path = Path({str(descendant_pid_path)!r})\n"
            "child = subprocess.Popen([sys.executable, descendant])\n"
            "pid_path.write_text(str(child.pid))\n"
            "print('wrapper:done', flush=True)\n"
        )
        stdout_log_path = self.repo_root / "stdout.log"
        stderr_log_path = self.repo_root / "stderr.log"

        started_at = time.monotonic()
        descendant_pid: int | None = None
        try:
            return_code = planner.run_command_with_output_logs(
                [sys.executable, str(wrapper_path)],
                cwd=self.repo_root,
                env=os.environ.copy(),
                stdout_log_path=stdout_log_path,
                stderr_log_path=stderr_log_path,
            )
            elapsed_seconds = time.monotonic() - started_at
            descendant_pid = int(descendant_pid_path.read_text())

            self.assertEqual(0, return_code)
            self.assertLess(elapsed_seconds, 2)
            self.assertEqual("wrapper:done\n", stdout_log_path.read_text())
        finally:
            if descendant_pid is None and descendant_pid_path.exists():
                descendant_pid = int(descendant_pid_path.read_text())
            if descendant_pid is not None:
                try:
                    os.kill(descendant_pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass

    @unittest.skipUnless(os.name == "posix", "process-group signals are POSIX-only")
    def test_logged_command_stays_in_supervisor_process_group(self) -> None:
        child_path = self.repo_root / "signal_child.py"
        ready_path = self.repo_root / "signal-child-ready.txt"
        terminated_path = self.repo_root / "signal-child-terminated.txt"
        child_path.write_text(
            "import os\n"
            "import signal\n"
            "import sys\n"
            "import time\n"
            "from pathlib import Path\n"
            "ready = Path(sys.argv[1])\n"
            "terminated = Path(sys.argv[2])\n"
            "def handle_sigterm(_signum, _frame):\n"
            "    terminated.write_text('terminated')\n"
            "    raise SystemExit(0)\n"
            "signal.signal(signal.SIGTERM, handle_sigterm)\n"
            "ready.write_text(f'{os.getpid()} {os.getpgrp()}')\n"
            "while True:\n"
            "    time.sleep(1)\n"
        )
        helper_path = self.repo_root / "signal_runner.py"
        helper_path.write_text(
            "import importlib.util\n"
            "import os\n"
            "import sys\n"
            "from pathlib import Path\n"
            f"planner_path = Path({str(PLANNER)!r})\n"
            "spec = importlib.util.spec_from_file_location('signal_test_planner', planner_path)\n"
            "if spec is None or spec.loader is None:\n"
            "    raise SystemExit('failed to load planner')\n"
            "planner = importlib.util.module_from_spec(spec)\n"
            "sys.modules[spec.name] = planner\n"
            "spec.loader.exec_module(planner)\n"
            f"child_path = {str(child_path)!r}\n"
            f"ready_path = {str(ready_path)!r}\n"
            f"terminated_path = {str(terminated_path)!r}\n"
            f"cwd = Path({str(self.repo_root)!r})\n"
            "planner.run_command_with_output_logs(\n"
            "    [sys.executable, child_path, ready_path, terminated_path],\n"
            "    cwd=cwd,\n"
            "    env=os.environ.copy(),\n"
            "    stdout_log_path=cwd / 'signal.stdout.log',\n"
            "    stderr_log_path=cwd / 'signal.stderr.log',\n"
            ")\n"
        )
        helper = subprocess.Popen(
            [sys.executable, str(helper_path)],
            cwd=self.repo_root,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
        child_pid: int | None = None
        try:
            deadline = time.monotonic() + 5
            while not ready_path.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(ready_path.exists(), "logged command did not start")
            child_pid, child_process_group = map(int, ready_path.read_text().split())
            self.assertEqual(helper.pid, child_process_group)

            os.killpg(helper.pid, signal.SIGTERM)
            helper.wait(timeout=5)
            deadline = time.monotonic() + 2
            while not terminated_path.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            stderr_output = helper.stderr.read() if helper.stderr is not None else ""
            self.assertTrue(terminated_path.exists(), stderr_output)
        finally:
            if helper.poll() is None:
                try:
                    os.killpg(helper.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                helper.wait(timeout=5)
            if child_pid is not None and not terminated_path.exists():
                try:
                    os.kill(child_pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            if helper.stdout is not None:
                helper.stdout.close()
            if helper.stderr is not None:
                helper.stderr.close()

    def write_resume_verify_fixture(self, count: int = 5) -> tuple[Path, Path, Path]:
        command_log = self.repo_root / "resume-command-log.txt"
        stub_path = self.repo_root / "resume_stub_command.py"
        stub_path.write_text(
            "import os\n"
            "import sys\n"
            "from pathlib import Path\n"
            "label = sys.argv[1]\n"
            "Path(os.environ['CARGO_VALIDATE_TEST_LOG']).open('a').write(label + '\\n')\n"
            "fail_labels = {item for item in os.environ.get('CARGO_VALIDATE_FAIL_LABELS', '').split(',') if item}\n"
            "if label in fail_labels:\n"
            "    raise SystemExit(7)\n"
        )
        command_names = [f"cmd-{index:02d}" for index in range(1, count + 1)]
        command_blocks = []
        for command_name in command_names:
            command_blocks.append(
                textwrap.dedent(f"""
                [commands.{command_name}]
                argv = ["{sys.executable}", "{stub_path}", "{command_name}"]
            """).strip()
            )
        config_path = self.repo_root / "resume-verify-config.toml"
        config_path.write_text(
            textwrap.dedent(f"""
            # Merge-safety anchor: test fixture for resumable cargo-validate verify behavior.
            schema_version = 1

            [defaults]
            standard_mode = "standard"
            unknown_rust_path_policy = "package-plus-cli-strict"
            workspace_features_policy = "deny-routine-all-features"

            [receipts]
            dir = "receipts"

            {chr(10).join(command_blocks)}

            [[path_rules]]
            patterns = ["fixture.txt"]
            commands = [{", ".join(json.dumps(command_name) for command_name in command_names)}]
        """).strip()
            + "\n"
        )
        metadata_path = self.repo_root / "empty-resume-metadata.json"
        metadata_path.write_text(json.dumps({"packages": []}))
        (self.repo_root / "fixture.txt").write_text("fixture\n")
        return config_path, metadata_path, command_log

    def run_resume_verify_fixture(
        self,
        config_path: Path,
        metadata_path: Path,
        command_log: Path,
        receipt_dir: Path,
        *extra_args: str,
        fail_labels: str = "",
    ) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        env["CARGO_VALIDATE_TEST_LOG"] = str(command_log)
        env["CARGO_VALIDATE_FAIL_LABELS"] = fail_labels
        return self.run_planner(
            "verify",
            "--file",
            "fixture.txt",
            "--mode",
            "standard",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(metadata_path),
            "--config",
            str(config_path),
            "--receipt-dir",
            str(receipt_dir),
            *extra_args,
            check=False,
            env=env,
        )

    def test_verify_resume_rejects_failed_partial_summary(self) -> None:
        config_path, metadata_path, command_log = self.write_resume_verify_fixture(
            count=5
        )
        receipt_dir = self.repo_root / "resume-receipts"

        first = self.run_resume_verify_fixture(
            config_path,
            metadata_path,
            command_log,
            receipt_dir,
            fail_labels="cmd-05",
        )
        self.assertEqual(7, first.returncode)

        second = self.run_resume_verify_fixture(
            config_path,
            metadata_path,
            command_log,
            receipt_dir,
            "--resume",
            "--explain-skip",
        )
        self.assertEqual(2, second.returncode)
        self.assertIn("coverage=full", second.stderr)
        self.assertEqual(
            ["cmd-01", "cmd-02", "cmd-03", "cmd-04", "cmd-05"],
            command_log.read_text().splitlines(),
        )

        run_entries = [
            json.loads(line)
            for line in (receipt_dir / "last-run.jsonl").read_text().splitlines()
        ]
        self.assertEqual(
            ["executed", "executed", "executed", "executed", "executed"],
            [entry["coverage"] for entry in run_entries],
        )
        self.assertEqual([0, 0, 0, 0, 7], [entry["status"] for entry in run_entries])
        summary = json.loads((receipt_dir / "last-run-summary.json").read_text())
        self.assertEqual("partial", summary["coverage"])

    def test_verify_resume_reruns_after_changed_input_digest(self) -> None:
        config_path, metadata_path, command_log = self.write_resume_verify_fixture(
            count=3
        )
        receipt_dir = self.repo_root / "input-digest-receipts"

        first = self.run_resume_verify_fixture(
            config_path, metadata_path, command_log, receipt_dir
        )
        self.assertEqual(0, first.returncode, first.stderr)
        (self.repo_root / "fixture.txt").write_text("fixture changed\n")
        second = self.run_resume_verify_fixture(
            config_path, metadata_path, command_log, receipt_dir, "--resume"
        )
        self.assertEqual(0, second.returncode, second.stderr)
        self.assertEqual(
            ["cmd-01", "cmd-02", "cmd-03", "cmd-01", "cmd-02", "cmd-03"],
            command_log.read_text().splitlines(),
        )

    def test_verify_resume_reruns_after_validation_tooling_digest_changes(self) -> None:
        config_path, metadata_path, command_log = self.write_resume_verify_fixture(
            count=2
        )
        receipt_dir = self.repo_root / "tooling-digest-receipts"

        first = self.run_resume_verify_fixture(
            config_path, metadata_path, command_log, receipt_dir
        )
        self.assertEqual(0, first.returncode, first.stderr)
        tooling_path = self.repo_root / "scripts" / "cargo-guard.sh"
        tooling_path.parent.mkdir(parents=True, exist_ok=True)
        tooling_path.write_text("# changed validation tooling\n")
        second = self.run_resume_verify_fixture(
            config_path, metadata_path, command_log, receipt_dir, "--resume"
        )
        self.assertEqual(0, second.returncode, second.stderr)
        self.assertEqual(
            ["cmd-01", "cmd-02", "cmd-01", "cmd-02"],
            command_log.read_text().splitlines(),
        )

    def test_verify_from_index_is_partial_and_does_not_reuse_full_summary(self) -> None:
        config_path, metadata_path, command_log = self.write_resume_verify_fixture(
            count=4
        )
        receipt_dir = self.repo_root / "from-index-receipts"
        receipt_dir.mkdir()
        (receipt_dir / "last-run-summary.json").write_text(
            json.dumps({"coverage": "full", "status": 0}) + "\n"
        )

        process = self.run_resume_verify_fixture(
            config_path,
            metadata_path,
            command_log,
            receipt_dir,
            "--from-index",
            "3",
            "--explain-skip",
        )
        self.assertEqual(0, process.returncode, process.stderr)
        self.assertEqual(["cmd-03", "cmd-04"], command_log.read_text().splitlines())
        self.assertIn("before --from-index 3", process.stdout)
        run_entries = [
            json.loads(line)
            for line in (receipt_dir / "last-run.jsonl").read_text().splitlines()
        ]
        self.assertEqual([None, None, 0, 0], [entry["status"] for entry in run_entries])
        self.assertEqual(
            ["from-index"] * 4, [entry["partial_mode"] for entry in run_entries]
        )
        summary = json.loads((receipt_dir / "last-run-summary.json").read_text())
        self.assertEqual("partial", summary["coverage"])
        self.assertEqual("from-index", summary["partial_mode"])

    def test_verify_resume_does_not_trust_from_index_partial_receipt(self) -> None:
        config_path, metadata_path, command_log = self.write_resume_verify_fixture(
            count=4
        )
        receipt_dir = self.repo_root / "from-index-then-resume-receipts"

        partial = self.run_resume_verify_fixture(
            config_path,
            metadata_path,
            command_log,
            receipt_dir,
            "--from-index",
            "3",
        )
        self.assertEqual(0, partial.returncode, partial.stderr)
        resume = self.run_resume_verify_fixture(
            config_path, metadata_path, command_log, receipt_dir, "--resume"
        )
        self.assertEqual(2, resume.returncode)
        self.assertIn("coverage=full", resume.stderr)
        self.assertEqual(["cmd-03", "cmd-04"], command_log.read_text().splitlines())
        run_entries = [
            json.loads(line)
            for line in (receipt_dir / "last-run.jsonl").read_text().splitlines()
        ]
        self.assertEqual(
            ["skipped", "skipped", "executed", "executed"],
            [entry["coverage"] for entry in run_entries],
        )
        self.assertEqual(
            ["from-index"] * 4, [entry["partial_mode"] for entry in run_entries]
        )
        summary = json.loads((receipt_dir / "last-run-summary.json").read_text())
        self.assertEqual("partial", summary["coverage"])

    def test_verify_from_index_rejects_index_past_plan(self) -> None:
        config_path, metadata_path, command_log = self.write_resume_verify_fixture(
            count=2
        )
        receipt_dir = self.repo_root / "from-index-past-plan-receipts"

        process = self.run_resume_verify_fixture(
            config_path,
            metadata_path,
            command_log,
            receipt_dir,
            "--from-index",
            "3",
        )
        self.assertEqual(2, process.returncode)
        self.assertIn("--from-index 3 exceeds plan length 2", process.stderr)
        self.assertFalse(command_log.exists())

    def test_verify_only_failed_is_partial(self) -> None:
        config_path, metadata_path, command_log = self.write_resume_verify_fixture(
            count=3
        )
        receipt_dir = self.repo_root / "only-failed-receipts"

        first = self.run_resume_verify_fixture(
            config_path,
            metadata_path,
            command_log,
            receipt_dir,
            fail_labels="cmd-02",
        )
        self.assertEqual(7, first.returncode)
        second = self.run_resume_verify_fixture(
            config_path,
            metadata_path,
            command_log,
            receipt_dir,
            "--only-failed",
            "--explain-skip",
        )
        self.assertEqual(0, second.returncode, second.stderr)
        self.assertEqual(
            ["cmd-01", "cmd-02", "cmd-03", "cmd-02"],
            command_log.read_text().splitlines(),
        )
        self.assertIn("previous matching run passed", second.stdout)
        run_entries = [
            json.loads(line)
            for line in (receipt_dir / "last-run.jsonl").read_text().splitlines()
        ]
        self.assertEqual(
            ["only-failed"] * 3, [entry["partial_mode"] for entry in run_entries]
        )
        summary = json.loads((receipt_dir / "last-run-summary.json").read_text())
        self.assertEqual("partial", summary["coverage"])
        self.assertEqual("only-failed", summary["partial_mode"])

    def test_verify_resume_does_not_trust_only_failed_partial_receipt(self) -> None:
        config_path, metadata_path, command_log = self.write_resume_verify_fixture(
            count=3
        )
        receipt_dir = self.repo_root / "only-failed-then-resume-receipts"

        first = self.run_resume_verify_fixture(
            config_path,
            metadata_path,
            command_log,
            receipt_dir,
            fail_labels="cmd-02",
        )
        self.assertEqual(7, first.returncode)
        partial = self.run_resume_verify_fixture(
            config_path, metadata_path, command_log, receipt_dir, "--only-failed"
        )
        self.assertEqual(0, partial.returncode, partial.stderr)
        resume = self.run_resume_verify_fixture(
            config_path, metadata_path, command_log, receipt_dir, "--resume"
        )
        self.assertEqual(2, resume.returncode)
        self.assertIn("coverage=full", resume.stderr)
        self.assertEqual(
            ["cmd-01", "cmd-02", "cmd-03", "cmd-02"],
            command_log.read_text().splitlines(),
        )
        run_entries = [
            json.loads(line)
            for line in (receipt_dir / "last-run.jsonl").read_text().splitlines()
        ]
        self.assertEqual(
            ["skipped", "executed", "skipped"],
            [entry["coverage"] for entry in run_entries],
        )
        self.assertEqual(
            ["only-failed"] * 3, [entry["partial_mode"] for entry in run_entries]
        )
        summary = json.loads((receipt_dir / "last-run-summary.json").read_text())
        self.assertEqual("partial", summary["coverage"])

    def test_verify_resume_does_not_trust_old_receipts_without_resume_ids(self) -> None:
        config_path, metadata_path, command_log = self.write_resume_verify_fixture(
            count=2
        )
        receipt_dir = self.repo_root / "old-receipt-reject-receipts"
        receipt_dir.mkdir()
        (receipt_dir / "last-run.jsonl").write_text(
            json.dumps({"index": 1, "argv": ["old"], "status": 0})
            + "\n"
            + json.dumps({"index": 2, "argv": ["old"], "status": 0})
            + "\n"
        )

        process = self.run_resume_verify_fixture(
            config_path, metadata_path, command_log, receipt_dir, "--resume"
        )
        self.assertEqual(2, process.returncode)
        self.assertIn("last-run-summary.json", process.stderr)
        self.assertFalse(command_log.exists())

    def test_verify_resume_rejects_in_progress_summary(self) -> None:
        config_path, metadata_path, command_log = self.write_resume_verify_fixture(
            count=2
        )
        receipt_dir = self.repo_root / "in-progress-receipts"

        first = self.run_resume_verify_fixture(
            config_path, metadata_path, command_log, receipt_dir
        )
        self.assertEqual(0, first.returncode, first.stderr)
        summary = json.loads((receipt_dir / "last-run-summary.json").read_text())
        summary["coverage"] = "in_progress"
        (receipt_dir / "last-run-summary.json").write_text(
            json.dumps(summary, sort_keys=True) + "\n"
        )

        second = self.run_resume_verify_fixture(
            config_path, metadata_path, command_log, receipt_dir, "--resume"
        )
        self.assertEqual(2, second.returncode)
        self.assertIn("coverage=full", second.stderr)
        self.assertEqual(["cmd-01", "cmd-02"], command_log.read_text().splitlines())

    def test_verify_resume_matches_command_key_not_command_id_only(self) -> None:
        config_path, metadata_path, command_log = self.write_resume_verify_fixture(
            count=2
        )
        receipt_dir = self.repo_root / "command-key-receipts"

        first = self.run_resume_verify_fixture(
            config_path, metadata_path, command_log, receipt_dir
        )
        self.assertEqual(0, first.returncode, first.stderr)
        prior_entries = [
            json.loads(line)
            for line in (receipt_dir / "last-run.jsonl").read_text().splitlines()
        ]
        misleading_entry = dict(prior_entries[1])
        misleading_entry["command_id"] = prior_entries[0]["command_id"]
        (receipt_dir / "last-run.jsonl").write_text(
            json.dumps(misleading_entry, sort_keys=True) + "\n"
        )

        second = self.run_resume_verify_fixture(
            config_path, metadata_path, command_log, receipt_dir, "--resume"
        )
        self.assertEqual(0, second.returncode, second.stderr)
        self.assertEqual(
            ["cmd-01", "cmd-02", "cmd-01"], command_log.read_text().splitlines()
        )
        run_entries = [
            json.loads(line)
            for line in (receipt_dir / "last-run.jsonl").read_text().splitlines()
        ]
        self.assertEqual(
            ["executed", "skipped"], [entry["coverage"] for entry in run_entries]
        )

    def test_codex_v8_target_config_is_host_only_and_guarded(self) -> None:
        planner = load_planner_module()
        cases = [
            (
                "unsupported target",
                {
                    "argv": ["./scripts/cargo-guard.sh", "cargo", "build"],
                    "codex_v8_target": "musl",
                },
                "codex_v8_target must be 'host'",
            ),
            (
                "unguarded command",
                {
                    "argv": [sys.executable, "fixture.py"],
                    "codex_v8_target": "host",
                },
                "requires a direct guarded Cargo build-like command",
            ),
        ]
        for name, command, expected_error in cases:
            with self.subTest(name=name):
                config = {
                    "schema_version": 1,
                    "defaults": {
                        "standard_mode": "standard",
                        "unknown_rust_path_policy": "disabled",
                        "workspace_features_policy": "deny-routine-all-features",
                    },
                    "commands": {"fixture": command},
                }
                with self.assertRaises(planner.PlannerError) as context:
                    planner.validate_config(config, [])
                self.assertIn(expected_error, str(context.exception))

    def test_codex_v8_runtime_env_uses_host_artifacts_and_workflow_cache(
        self,
    ) -> None:
        planner = load_planner_module()
        command = planner.CommandEntry(
            argv=("./scripts/cargo-guard.sh", "cargo", "build"),
            reason="fixture",
            codex_v8_target="host",
        )
        resolved_env = {
            "RUSTY_V8_ARCHIVE": "/cache/archive.gz",
            "RUSTY_V8_SRC_BINDING_PATH": "/cache/binding.rs",
        }
        with (
            mock.patch.object(
                planner, "rustc_host_target", return_value="x86_64-unknown-linux-gnu"
            ),
            mock.patch.object(
                planner,
                "resolve_codex_v8_cargo_env",
                return_value=resolved_env,
            ) as resolver,
            mock.patch.object(planner.Path, "home", return_value=Path("/home/test")),
        ):
            actual_env, cleared_env = planner.resolve_codex_v8_command_env(command)

        self.assertEqual(resolved_env, actual_env)
        self.assertEqual(planner.CODEX_V8_INHERITED_ENV_KEYS, cleared_env)
        resolver.assert_called_once_with(
            planner.TARGET_SPECS["x86_64-unknown-linux-gnu"],
            environ={},
            cache_root=Path("/home/test/.cache/codex/cargo-validation/rusty-v8"),
        )

    def test_verify_applies_and_records_resolved_codex_v8_env(self) -> None:
        planner = load_planner_module()
        config_path, metadata_path, command_log = self.write_direct_guard_fixture()
        config_path.write_text(
            config_path.read_text().replace(
                'profile = "check"\n',
                'profile = "check"\ncodex_v8_target = "host"\n',
            )
        )
        receipt_dir = self.repo_root / "v8-env-receipts"
        config = planner.load_config(config_path)
        packages = planner.load_metadata(self.repo_root, metadata_path)
        plan = planner.build_plan(
            action="verify",
            stage="validation",
            mode="quick",
            files=["fixture.txt"],
            explicit_surfaces=[],
            repo_root=self.repo_root,
            config=config,
            packages=packages,
            receipt_dir=receipt_dir,
            telemetry_level="off",
        )
        v8_log = self.repo_root / "v8-env-log.txt"
        resolved_env = {
            "RUSTY_V8_ARCHIVE": "/cache/canonical-archive.gz",
            "RUSTY_V8_SRC_BINDING_PATH": "/cache/canonical-binding.rs",
        }
        with (
            mock.patch.object(
                planner,
                "resolve_codex_v8_command_env",
                return_value=(resolved_env, planner.CODEX_V8_INHERITED_ENV_KEYS),
            ),
            mock.patch.dict(
                os.environ,
                {
                    "CARGO_VALIDATE_TEST_LOG": str(command_log),
                    "CARGO_VALIDATE_V8_TEST_LOG": str(v8_log),
                    "RUSTY_V8_ARCHIVE": "/cache/inherited-archive.gz",
                    "RUSTY_V8_MIRROR": "https://invalid.example",
                    "RUSTY_V8_SRC_BINDING_PATH": "/cache/inherited-binding.rs",
                    "V8_FROM_SOURCE": "1",
                },
            ),
        ):
            status = planner.verify_plan(plan, self.repo_root, keep_going=False)

        self.assertEqual(0, status)
        self.assertEqual(
            "/cache/canonical-archive.gz|/cache/canonical-binding.rs||\n",
            v8_log.read_text(),
        )
        run_entries = [
            json.loads(line)
            for line in (receipt_dir / "last-run.jsonl").read_text().splitlines()
        ]
        self.assertEqual(1, len(run_entries))
        self.assertEqual("host", run_entries[0]["codex_v8_target"])
        self.assertEqual(resolved_env, run_entries[0]["resolved_env"])
        self.assertEqual(
            list(planner.CODEX_V8_INHERITED_ENV_KEYS), run_entries[0]["cleared_env"]
        )
        self.assertNotIn("runtime_env_error", run_entries[0])

    def test_verify_records_codex_v8_runtime_env_failure_without_running_command(
        self,
    ) -> None:
        planner = load_planner_module()
        config_path, metadata_path, command_log = self.write_direct_guard_fixture()
        config_path.write_text(
            config_path.read_text().replace(
                'profile = "check"\n',
                'profile = "check"\ncodex_v8_target = "host"\n',
            )
        )
        receipt_dir = self.repo_root / "v8-env-failure-receipts"
        config = planner.load_config(config_path)
        packages = planner.load_metadata(self.repo_root, metadata_path)
        plan = planner.build_plan(
            action="verify",
            stage="validation",
            mode="quick",
            files=["fixture.txt"],
            explicit_surfaces=[],
            repo_root=self.repo_root,
            config=config,
            packages=packages,
            receipt_dir=receipt_dir,
            telemetry_level="off",
        )
        error = "failed to prepare checksum-verified V8 artifacts"
        with mock.patch.object(
            planner,
            "resolve_codex_v8_command_env",
            side_effect=planner.PlannerError(error),
        ):
            status = planner.verify_plan(plan, self.repo_root, keep_going=False)

        self.assertEqual(2, status)
        self.assertFalse(command_log.exists())
        run_entries = [
            json.loads(line)
            for line in (receipt_dir / "last-run.jsonl").read_text().splitlines()
        ]
        self.assertEqual(1, len(run_entries))
        self.assertEqual(
            {
                "coverage": "setup_failed",
                "coverage_source": "runtime-env-setup",
                "runtime_env_error": error,
                "status": 2,
            },
            {
                key: run_entries[0][key]
                for key in (
                    "coverage",
                    "coverage_source",
                    "runtime_env_error",
                    "status",
                )
            },
        )
        summary = json.loads((receipt_dir / "last-run-summary.json").read_text())
        self.assertEqual(
            [
                {
                    "index": 1,
                    "argv": list(plan.commands[0].argv),
                    "error": error,
                }
            ],
            summary["runtime_env_failures"],
        )

    def test_verify_command_env_overrides_inherited_env(self) -> None:
        command_log = self.repo_root / "env-command-log.txt"
        stub_path = self.repo_root / "env_stub.py"
        stub_path.write_text(
            "import os\n"
            "from pathlib import Path\n"
            'Path(os.environ["CARGO_VALIDATE_TEST_LOG"]).write_text(\n'
            '    os.environ.get("CARGO_GUARD_RESOURCE_PROFILE", "") + "|"\n'
            '    + os.environ.get("CARGO_GUARD_EXPECTED_GROWTH_GIB", "") + "|"\n'
            '    + os.environ.get("CARGO_GUARD_MONITOR", "") + "\\n"\n'
            ")\n"
        )
        config_path = self.repo_root / "env-config.toml"
        config_path.write_text(
            textwrap.dedent(f"""
            schema_version = 1

            [defaults]
            standard_mode = "standard"
            unknown_rust_path_policy = "package-plus-cli-strict"
            workspace_features_policy = "deny-routine-all-features"

            [receipts]
            dir = "receipts"

            [resource_profiles.check]
            reserve_free_pct = 15
            reserve_free_gib = 12
            abort_free_pct = 6
            abort_free_gib = 6
            monitor = false

            [commands.env-check]
            argv = ["{sys.executable}", "{stub_path}"]
            profile = "check"

            [[path_rules]]
            patterns = ["fixture.txt"]
            commands = ["env-check"]
        """).strip()
            + "\n"
        )
        metadata_path = self.repo_root / "empty-metadata.json"
        metadata_path.write_text(json.dumps({"packages": []}))
        (self.repo_root / "fixture.txt").write_text("fixture\n")
        env = os.environ.copy()
        env["CARGO_VALIDATE_TEST_LOG"] = str(command_log)
        env["CARGO_GUARD_RESOURCE_PROFILE"] = "inherited"
        env["CARGO_GUARD_EXPECTED_GROWTH_GIB"] = "999"
        env["CARGO_GUARD_MONITOR"] = "1"
        process = self.run_planner(
            "verify",
            "--file",
            "fixture.txt",
            "--mode",
            "standard",
            "--repo-root",
            str(self.repo_root),
            "--metadata-json",
            str(metadata_path),
            "--config",
            str(config_path),
            "--receipt-dir",
            str(self.repo_root / "env-receipts"),
            check=False,
            env=env,
        )
        self.assertEqual(0, process.returncode)
        self.assertEqual("check||0\n", command_log.read_text())


if __name__ == "__main__":
    unittest.main()
