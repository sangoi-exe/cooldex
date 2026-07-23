#!/usr/bin/env python3
"""Fixture tests for rust-blast-radius-guard.py."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("rust-blast-radius-guard.py")


class RustBlastRadiusGuardTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.workspace = Path(self.temp_dir.name)
        self.write_fixture_workspace()

    def tearDown(self) -> None:
        self.temp_dir.cleanup()

    def write_fixture_workspace(self) -> None:
        (self.workspace / "src").mkdir()
        (self.workspace / "tests").mkdir()
        (self.workspace / "docs").mkdir()
        (self.workspace / "config").mkdir()
        helper_calls = "\n".join(
            f"                        helper_{index}();" for index in range(45)
        )
        (self.workspace / "AGENTS.md").write_text(
            textwrap.dedent(
                """\
                # Fixture Atlas

                - `Widget` ownership: open `src/lib.rs` before touching widget execution.
                - Widget runner fanout includes `src/use_widget.rs`, `tests/widget_flow.rs`,
                  `docs/widget.md`, and `config/widget.schema.json`.
                """
            ),
            encoding="utf-8",
        )
        (self.workspace / "src" / "lib.rs").write_text(
            textwrap.dedent(
                """\
                pub struct Widget;
                pub struct WidgetConfig;
                pub struct WidgetError;

                pub trait Runner {
                    fn run(&self);
                }

                impl Widget {
                    pub fn run(&self, config: WidgetConfig) -> Result<(), WidgetError> {
                        self.align(config);
                        helper();
__HELPER_CALLS__
                        Ok(())
                    }

                    fn align(&self, _config: WidgetConfig) {}

                    pub fn risky(&self, runner: Box<dyn Runner>) {
                        #[cfg(feature = "nightly")]
                        runner.run();
                    }
                }

                pub fn helper() {}
                """
            ).replace("__HELPER_CALLS__", helper_calls),
            encoding="utf-8",
        )
        for skipped_dir in (
            ".git",
            ".hg",
            ".sangoi",
            ".svn",
            "__pycache__",
            "target",
            "node_modules",
            "dist",
            "build",
            ".next",
            ".venv",
            "venv",
        ):
            skipped_path = self.workspace / skipped_dir
            skipped_path.mkdir()
            (skipped_path / "ignored.rs").write_text(
                "fn ignored(widget: Widget) { let _ = widget.run(WidgetConfig); }\n",
                encoding="utf-8",
            )
        (self.workspace / "src" / "other.rs").write_text(
            "pub fn helper() {}\n",
            encoding="utf-8",
        )
        (self.workspace / "src" / "actions.rs").write_text(
            "pub fn run() {}\n",
            encoding="utf-8",
        )
        (self.workspace / "src" / "use_actions.rs").write_text(
            "pub fn call_action() { crate::actions::run(); }\n",
            encoding="utf-8",
        )
        (self.workspace / "src" / "use_widget.rs").write_text(
            textwrap.dedent(
                """\
                use crate::{Widget, WidgetConfig};

                pub fn call_widget(widget: Widget) {
                    let _ = widget.run(WidgetConfig);
                }
                """
            ),
            encoding="utf-8",
        )
        (self.workspace / "tests" / "widget_flow.rs").write_text(
            textwrap.dedent(
                """\
                use fixture::{Widget, WidgetConfig};

                #[test]
                fn widget_run_flow() {
                    let widget = Widget;
                    let _ = widget.run(WidgetConfig);
                }
                """
            ),
            encoding="utf-8",
        )
        (self.workspace / "docs" / "widget.md").write_text(
            "Changing Widget::run requires docs and schema checks.\n",
            encoding="utf-8",
        )
        (self.workspace / "config" / "widget.schema.json").write_text(
            '{"title":"Widget::run schema"}\n',
            encoding="utf-8",
        )

    def run_guard(
        self,
        *args: str,
        env: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        command = [
            sys.executable,
            str(SCRIPT),
            "--workspace",
            str(self.workspace),
            *args,
        ]
        return subprocess.run(
            command, text=True, capture_output=True, check=False, env=env
        )

    def run_json(
        self, *args: str, env: dict[str, str] | None = None
    ) -> dict[str, object]:
        completed = self.run_guard(*args, "--json", env=env)
        self.assertEqual(completed.returncode, 0, completed.stderr + completed.stdout)
        return json.loads(completed.stdout)

    def test_symbol_report_contains_required_sections_and_buckets(self) -> None:
        completed = self.run_guard("--symbol", "Widget::run", "--max-hits", "5")
        self.assertEqual(completed.returncode, 0, completed.stderr + completed.stdout)
        for section in (
            "## Target Resolution",
            "## Owner/Declaration",
            "## Inbound References",
            "## Outbound Tokens",
            "## Atlas/Customization Candidate Overlays",
            "## High-Risk Unknowns",
            "## Suggested Next Commands",
        ):
            self.assertIn(section, completed.stdout)
        self.assertIn("Widget::run", completed.stdout)
        self.assertIn("rust_production", completed.stdout)
        self.assertIn("rust_tests_fixtures", completed.stdout)
        self.assertIn("docs_schema_config_instructions", completed.stdout)
        self.assertIn("helper_44", completed.stdout)
        self.assertIn("calls (", completed.stdout)

    def test_json_output_has_material_fact_parity(self) -> None:
        report = self.run_json("--symbol", "Widget::run", "--max-hits", "5")
        for key in (
            "target",
            "backend",
            "owner_declarations",
            "inbound_references",
            "outbound_tokens",
            "atlas_candidate_overlays",
            "high_risk_unknowns",
            "suggested_next_commands",
            "truncation",
        ):
            self.assertIn(key, report)
        self.assertEqual(report["status"], "resolved")
        self.assertEqual(report["target"]["resolved_symbol"], "Widget::run")
        self.assertIn(report["backend"], {"rg", "python-fallback"})

    def test_default_output_is_uncapped(self) -> None:
        report = self.run_json("--symbol", "Widget::run")
        for bucket in report["inbound_references"].values():
            self.assertEqual(bucket["shown"], bucket["total"])
            self.assertEqual(len(bucket["items"]), bucket["total"])
        outbound = report["outbound_tokens"]
        self.assertGreater(len(outbound["calls"]), 40)
        self.assertIn("helper_44", outbound["calls"])
        self.assertNotIn("truncated", outbound)
        self.assertEqual(outbound["totals"]["calls"], len(outbound["calls"]))
        overlays = report["atlas_candidate_overlays"]
        self.assertEqual(overlays["shown"], overlays["total"])
        self.assertEqual(len(overlays["items"]), overlays["total"])
        self.assertEqual(report["truncation"]["buckets_capped"], [])
        self.assertFalse(report["truncation"]["atlas_overlays_capped"])

    def test_rg_backend_honors_skipped_dirs(self) -> None:
        report = self.run_json("--symbol", "Widget::run")
        skipped_dirs = {
            ".git",
            ".hg",
            ".sangoi",
            ".svn",
            "__pycache__",
            "target",
            "node_modules",
            "dist",
            "build",
            ".next",
            ".venv",
            "venv",
        }
        for bucket in report["inbound_references"].values():
            for item in bucket["items"]:
                path_parts = Path(item["path"]).parts
                self.assertFalse(skipped_dirs.intersection(path_parts), item["path"])

    def test_depth_option_is_not_available_without_real_recursive_graph(self) -> None:
        completed = self.run_guard("--symbol", "Widget::run", "--depth", "1")
        self.assertEqual(completed.returncode, 2)
        self.assertIn("unrecognized arguments: --depth", completed.stderr)

    def test_file_line_resolution(self) -> None:
        lines = (
            (self.workspace / "src" / "lib.rs").read_text(encoding="utf-8").splitlines()
        )
        run_line = next(
            index
            for index, line in enumerate(lines, start=1)
            if "pub fn run(&self" in line
        )
        report = self.run_json("--file", "src/lib.rs", "--line", str(run_line))
        self.assertEqual(report["target"]["resolved_symbol"], "Widget::run")
        self.assertEqual(report["target"]["requested_line"], run_line)

    def test_include_exact_file_keeps_ancestors_for_symbol_and_file_line(self) -> None:
        lines = (
            (self.workspace / "src" / "lib.rs").read_text(encoding="utf-8").splitlines()
        )
        run_line = next(
            index
            for index, line in enumerate(lines, start=1)
            if "pub fn run(&self" in line
        )
        symbol_report = self.run_json(
            "--symbol", "Widget::run", "--include", "src/lib.rs"
        )
        self.assertEqual(symbol_report["target"]["resolved_symbol"], "Widget::run")

        line_report = self.run_json(
            "--file", "src/lib.rs", "--line", str(run_line), "--include", "src/lib.rs"
        )
        self.assertEqual(line_report["target"]["resolved_symbol"], "Widget::run")

    def test_include_nested_directory_keeps_resolution_and_filtered_hits(self) -> None:
        lines = (
            (self.workspace / "src" / "lib.rs").read_text(encoding="utf-8").splitlines()
        )
        run_line = next(
            index
            for index, line in enumerate(lines, start=1)
            if "pub fn run(&self" in line
        )
        symbol_report = self.run_json("--symbol", "Widget::run", "--include", "src")
        symbol_paths = {
            item["path"]
            for item in symbol_report["inbound_references"]["rust_production"]["items"]
        }
        self.assertIn("src/use_widget.rs", symbol_paths)

        line_report = self.run_json(
            "--file", "src/lib.rs", "--line", str(run_line), "--include", "src"
        )
        line_paths = {
            item["path"]
            for item in line_report["inbound_references"]["rust_production"]["items"]
        }
        self.assertIn("src/use_widget.rs", line_paths)

    def test_file_line_short_free_function_finds_module_qualified_caller(self) -> None:
        lines = (
            (self.workspace / "src" / "actions.rs")
            .read_text(encoding="utf-8")
            .splitlines()
        )
        run_line = next(
            index for index, line in enumerate(lines, start=1) if "pub fn run" in line
        )
        report = self.run_json("--file", "src/actions.rs", "--line", str(run_line))
        production_paths = {
            item["path"]
            for item in report["inbound_references"]["rust_production"]["items"]
        }
        self.assertIn("src/use_actions.rs", production_paths)
        self.assertEqual(report["target"]["resolved_symbol"], "run")

    def test_python_fallback_uses_same_hit_shape(self) -> None:
        env = dict(os.environ)
        env["PATH"] = ""
        report = self.run_json("--symbol", "Widget::run", env=env)
        self.assertEqual(report["backend"], "python-fallback")
        production_items = report["inbound_references"]["rust_production"]["items"]
        self.assertTrue(production_items)
        self.assertIn("matched_terms", production_items[0])

    def test_ambiguous_free_function_exits_two_with_candidates(self) -> None:
        completed = self.run_guard("--symbol", "helper", "--json")
        self.assertEqual(completed.returncode, 2, completed.stderr + completed.stdout)
        report = json.loads(completed.stdout)
        self.assertEqual(report["status"], "ambiguous")
        self.assertGreaterEqual(len(report["target"]["candidates"]), 2)
        self.assertEqual(
            report["target"]["candidate_shown"], report["target"]["candidate_total"]
        )

    def test_capped_ambiguous_target_exposes_candidate_counts_and_warning(self) -> None:
        completed = self.run_guard("--symbol", "helper", "--max-hits", "1", "--json")
        self.assertEqual(completed.returncode, 2, completed.stderr + completed.stdout)
        report = json.loads(completed.stdout)
        self.assertEqual(report["status"], "ambiguous")
        self.assertEqual(report["target"]["candidate_shown"], 1)
        self.assertGreater(report["target"]["candidate_total"], 1)
        self.assertEqual(len(report["target"]["candidates"]), 1)
        self.assertIn("target candidates capped at 1 of", completed.stderr)

    def test_strict_high_risk_unknown_exits_three(self) -> None:
        completed = self.run_guard("--symbol", "Widget::risky", "--strict", "--json")
        self.assertEqual(completed.returncode, 3, completed.stderr + completed.stdout)
        report = json.loads(completed.stdout)
        kinds = {item["kind"] for item in report["high_risk_unknowns"]}
        self.assertIn("dynamic-dispatch", kinds)
        self.assertIn("cfg-gated-target-body", kinds)

    def test_agents_overlay_has_line_and_reason(self) -> None:
        report = self.run_json("--symbol", "Widget::run", "--max-hits", "5")
        overlays = report["atlas_candidate_overlays"]["items"]
        self.assertTrue(overlays)
        first_overlay = overlays[0]
        self.assertEqual(first_overlay["relpath"], "AGENTS.md")
        self.assertGreater(first_overlay["line"], 0)
        self.assertTrue(first_overlay["reasons"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
