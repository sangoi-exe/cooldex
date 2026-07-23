#!/usr/bin/env python3
"""Additional item-resolution fixture tests for rust-blast-radius-guard.py."""

import json
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("rust-blast-radius-guard.py")


class RustBlastRadiusGuardItemResolutionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.workspace = Path(self.temp_dir.name)
        (self.workspace / "src").mkdir()
        (self.workspace / "src" / "actions.rs").write_text(
            "pub fn run() {}\n", encoding="utf-8"
        )
        (self.workspace / "src" / "lib.rs").write_text(
            textwrap.dedent(
                """\
                use crate::actions::run as action_run;

                pub const DEFAULT_PORT: u16 = 1455;
                pub static GLOBAL_WIDGET_LABEL: &str = "widget";
                pub type WidgetAlias = Widget;
                pub use WidgetAlias as PublicWidgetAlias;

                pub struct Widget;
                pub struct FieldWidget {
                    pub strict_codex_scope: bool,
                }

                pub trait Runner {
                    fn run(&self);
                }

                pub trait WidgetView {
                    fn strict_codex_scope(&self) -> bool;
                }

                impl WidgetView for FieldWidget {
                    fn strict_codex_scope(&self) -> bool {
                        self.strict_codex_scope
                    }
                }

                impl Widget {
                    pub fn risky(&self, runner: Box<dyn Runner>) {
                        #[cfg(unix)]
                        runner.run();
                    }
                }
                """
            ),
            encoding="utf-8",
        )

    def tearDown(self) -> None:
        self.temp_dir.cleanup()

    def run_guard(self, *args: str) -> subprocess.CompletedProcess[str]:
        command = [
            sys.executable,
            str(SCRIPT),
            "--workspace",
            str(self.workspace),
            *args,
        ]
        return subprocess.run(command, text=True, capture_output=True, check=False)

    def run_json(self, *args: str) -> dict[str, object]:
        completed = self.run_guard(*args, "--json")
        self.assertEqual(completed.returncode, 0, completed.stderr + completed.stdout)
        return json.loads(completed.stdout)

    def line_containing(self, needle: str) -> int:
        lines = (
            (self.workspace / "src" / "lib.rs").read_text(encoding="utf-8").splitlines()
        )
        return next(
            index for index, line in enumerate(lines, start=1) if needle in line
        )

    def test_file_line_resolves_non_function_items(self) -> None:
        cases = {
            "DEFAULT_PORT": "const",
            "GLOBAL_WIDGET_LABEL": "static",
            "WidgetAlias = Widget": "type_alias",
            "strict_codex_scope: bool": "field",
            "use crate::actions::run": "use",
            "pub use WidgetAlias": "reexport",
        }
        for needle, expected_kind in cases.items():
            with self.subTest(needle=needle):
                report = self.run_json(
                    "--file", "src/lib.rs", "--line", str(self.line_containing(needle))
                )
                self.assertEqual(report["target"]["resolved_kind"], expected_kind)

    def test_ambiguous_symbol_emits_candidate_commands(self) -> None:
        completed = self.run_guard("--symbol", "strict_codex_scope", "--json")
        self.assertEqual(completed.returncode, 2, completed.stderr + completed.stdout)
        report = json.loads(completed.stdout)
        self.assertEqual(report["status"], "ambiguous")
        candidates = report["target"]["candidates"]
        kinds = {candidate["kind"] for candidate in candidates}
        self.assertIn("field", kinds)
        self.assertIn("trait_method", kinds)
        self.assertIn("method", kinds)
        for candidate in candidates:
            self.assertIn("--file", candidate["suggested_command"])
            self.assertIn("--line", candidate["suggested_command"])

    def test_unresolved_owner_mismatch_reports_leaf_candidates(self) -> None:
        completed = self.run_guard("--symbol", "WidgetSession::risky", "--json")
        self.assertEqual(completed.returncode, 2, completed.stderr + completed.stdout)
        report = json.loads(completed.stdout)
        self.assertEqual(report["status"], "unresolved")
        self.assertIn(
            "Widget::risky",
            {
                candidate["qualified_name"]
                for candidate in report["target"]["candidates"]
            },
        )
        self.assertTrue(report["suggested_next_commands"])

    def test_summary_write_report_keeps_uncapped_evidence_out_of_stdout(self) -> None:
        report_path = self.workspace / ".sangoi" / "guard" / "widget-risky.json"
        completed = self.run_guard(
            "--symbol",
            "Widget::risky",
            "--write-report",
            str(report_path.relative_to(self.workspace)),
            "--summary",
        )
        self.assertEqual(completed.returncode, 0, completed.stderr + completed.stdout)
        self.assertIn("# Rust Blast Radius Guard Summary", completed.stdout)
        self.assertIn("report_sha256", completed.stdout)
        report = json.loads(report_path.read_text(encoding="utf-8"))
        self.assertEqual(report["target"]["resolved_symbol"], "Widget::risky")
        self.assertTrue(report["report"]["uncapped"])

    def test_high_risk_unknowns_include_follow_up_commands(self) -> None:
        completed = self.run_guard("--symbol", "Widget::risky", "--strict", "--json")
        self.assertEqual(completed.returncode, 3, completed.stderr + completed.stdout)
        report = json.loads(completed.stdout)
        kinds = {item["kind"] for item in report["high_risk_unknowns"]}
        self.assertIn("dynamic-dispatch", kinds)
        self.assertIn("cfg-gated-target-body", kinds)
        for item in report["high_risk_unknowns"]:
            self.assertTrue(item["follow_up_commands"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
