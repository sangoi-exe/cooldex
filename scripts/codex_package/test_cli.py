#!/usr/bin/env python3

import argparse
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package.cli import main
from codex_package.cli import parse_args
from codex_package.cli import parse_package_version
from codex_package.targets import TARGET_SPECS
from codex_package.targets import resolve_computer_use_input_pair


class PackageVersionTest(unittest.TestCase):
    def test_accepts_release_prerelease_and_build_versions(self) -> None:
        for version in (
            "0.0.0",
            "1.2.3",
            "0.0.0-internal.deadbeef",
            "1.2.3-alpha.1+build.01",
            "18446744073709551615.0.0",
        ):
            with self.subTest(version=version):
                self.assertEqual(parse_package_version(version), version)

    def test_rejects_versions_the_runtime_cannot_parse(self) -> None:
        for version in (
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "v1.2.3",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.3-",
            "1.2.3-alpha..1",
            "1.2.3-01",
            "1.2.3+",
            "1.2.3+build..1",
            "18446744073709551616.0.0",
        ):
            with self.subTest(version=version):
                with self.assertRaises(argparse.ArgumentTypeError):
                    parse_package_version(version)


class ComputerUseInputTest(unittest.TestCase):
    def test_parses_complete_computer_use_pair(self) -> None:
        with patch.object(
            sys,
            "argv",
            [
                "build_codex_package.py",
                "--computer-use-mcp-bin",
                "mcp",
                "--sky-bin",
                "sky",
            ],
        ):
            args = parse_args()

        self.assertEqual(args.computer_use_mcp_bin, Path("mcp"))
        self.assertEqual(args.sky_bin, Path("sky"))

    def test_rejects_one_sided_computer_use_pair(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "specified together"):
            resolve_computer_use_input_pair(
                TARGET_SPECS["x86_64-unknown-linux-gnu"],
                computer_use_mcp_bin=Path("mcp"),
                sky_bin=None,
            )

    def test_rejects_computer_use_pair_for_unsupported_target(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "only supported for target"):
            resolve_computer_use_input_pair(
                TARGET_SPECS["aarch64-unknown-linux-gnu"],
                computer_use_mcp_bin=Path("mcp"),
                sky_bin=Path("sky"),
            )

    def test_validates_computer_use_pair_as_prebuilt_executables(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            computer_use_mcp_bin = root / "codex-computer-use-mcp"
            sky_bin = root / "sky_linux_x64"
            computer_use_mcp_bin.touch()
            sky_bin.touch()

            with self.assertRaisesRegex(RuntimeError, "is not executable"):
                resolve_computer_use_input_pair(
                    TARGET_SPECS["x86_64-unknown-linux-gnu"],
                    computer_use_mcp_bin=computer_use_mcp_bin,
                    sky_bin=sky_bin,
                )

            computer_use_mcp_bin.chmod(0o755)
            sky_bin.chmod(0o755)
            self.assertEqual(
                resolve_computer_use_input_pair(
                    TARGET_SPECS["x86_64-unknown-linux-gnu"],
                    computer_use_mcp_bin=computer_use_mcp_bin,
                    sky_bin=sky_bin,
                ),
                (computer_use_mcp_bin.resolve(), sky_bin.resolve()),
            )

    def test_rejects_archive_output_for_computer_use_pair_before_cargo(self) -> None:
        args = argparse.Namespace(
            target="x86_64-unknown-linux-gnu",
            variant="codex",
            package_dir=None,
            archive_output=[Path("codex.tar.gz")],
            computer_use_mcp_bin=Path("mcp"),
            sky_bin=Path("sky"),
        )
        with (
            patch("codex_package.cli.parse_args", return_value=args),
            patch(
                "codex_package.cli.resolve_computer_use_input_pair",
                return_value=(Path("mcp"), Path("sky")),
            ),
            patch("codex_package.cli.build_source_binaries") as build_source_binaries,
        ):
            with self.assertRaisesRegex(RuntimeError, "archive output"):
                main()

        build_source_binaries.assert_not_called()


if __name__ == "__main__":
    unittest.main()
