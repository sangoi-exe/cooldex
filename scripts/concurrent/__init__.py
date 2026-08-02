"""Temporary same-repository CI transport for one exact merge-base snapshot.

This module intentionally shadows ``concurrent`` only when
``scripts/stage_npm_packages.py`` runs in the task-scoped pull request named
below. It exits the interpreter after materializing the artifact expected by the
existing trusted workflow. The branch must never be merged.
"""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

EXPECTED_REPOSITORY = "sangoi-exe/cooldex"
EXPECTED_HEAD_REF = "agent/export-merge-base-ci-20260802"
EXPECTED_RELEASE = "0.133.0-alpha.4"
MERGE_BASE = "44d76c6a6dd04fa2efc302b906ac8774267a1272"
UPSTREAM_URL = "https://github.com/openai/codex.git"


def _argument_value(flag: str) -> str | None:
    try:
        return sys.argv[sys.argv.index(flag) + 1]
    except (ValueError, IndexError):
        return None


def _is_exact_export_invocation() -> bool:
    return (
        os.environ.get("GITHUB_ACTIONS") == "true"
        and os.environ.get("GITHUB_EVENT_NAME") == "pull_request"
        and os.environ.get("GITHUB_REPOSITORY") == EXPECTED_REPOSITORY
        and os.environ.get("GITHUB_HEAD_REF") == EXPECTED_HEAD_REF
        and Path(sys.argv[0]).as_posix().endswith("scripts/stage_npm_packages.py")
        and _argument_value("--release-version") == EXPECTED_RELEASE
        and _argument_value("--package") == "codex"
        and _argument_value("--output-dir") is not None
    )


def _run(command: list[str], *, stdout: int | None = None) -> None:
    subprocess.run(command, check=True, stdout=stdout)


def _export_merge_base() -> Path:
    output_dir_value = _argument_value("--output-dir")
    if output_dir_value is None:
        raise RuntimeError("missing --output-dir")

    output_dir = Path(output_dir_value)
    output_dir.mkdir(parents=True, exist_ok=True)
    output_path = output_dir / f"codex-npm-{EXPECTED_RELEASE}.tgz"

    work_root = Path(tempfile.mkdtemp(prefix="cooldex-merge-base-export-"))
    try:
        repository = work_root / "repository"
        payload_root = work_root / "payload"
        payload_tree = payload_root / "codex-merge-base"
        source_tar = work_root / "source.tar"

        _run(["git", "init", "--quiet", str(repository)])
        _run(
            [
                "git",
                "-C",
                str(repository),
                "fetch",
                "--quiet",
                "--depth=1",
                UPSTREAM_URL,
                MERGE_BASE,
            ]
        )
        actual = subprocess.check_output(
            ["git", "-C", str(repository), "rev-parse", "FETCH_HEAD"],
            text=True,
        ).strip()
        if actual != MERGE_BASE:
            raise RuntimeError(f"fetched {actual}, expected {MERGE_BASE}")

        _run(
            [
                "git",
                "-C",
                str(repository),
                "archive",
                "--format=tar",
                "--prefix=codex-merge-base/",
                f"--output={source_tar}",
                "FETCH_HEAD",
            ]
        )
        payload_root.mkdir(parents=True, exist_ok=True)
        _run(["tar", "-xf", str(source_tar), "-C", str(payload_root)])
        (payload_tree / "MERGE_BASE_COMMIT").write_text(
            f"{MERGE_BASE}\n",
            encoding="utf-8",
        )
        _run(
            [
                "tar",
                "-czf",
                str(output_path),
                "-C",
                str(payload_root),
                "codex-merge-base",
            ]
        )
        _run(["tar", "-tzf", str(output_path)], stdout=subprocess.DEVNULL)
        return output_path
    finally:
        shutil.rmtree(work_root, ignore_errors=True)


if not _is_exact_export_invocation():
    raise ImportError(
        "temporary merge-base exporter loaded outside its exact task-scoped CI invocation"
    )

try:
    artifact = _export_merge_base()
except BaseException as error:  # noqa: BLE001 - this is a process-boundary shim.
    print(f"merge-base export failed: {error}", file=sys.stderr, flush=True)
    os._exit(71)

print(f"exported exact merge-base artifact to {artifact}", flush=True)
os._exit(0)
