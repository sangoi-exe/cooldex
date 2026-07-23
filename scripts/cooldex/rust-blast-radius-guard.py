#!/usr/bin/env python3
"""Report a conservative Rust symbol blast radius for Codex/Cooldex workspaces.

The guard is intentionally not a Rust compiler or LSP replacement. It resolves a
Rust item, searches nearby textual fanout, and reports uncertainty explicitly so
agents do not confuse partial static evidence with proof.
"""

from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Iterable, Sequence, TypeVar


SKIPPED_DIR_NAMES = {
    ".git",
    ".sangoi",
    ".hg",
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

SEARCHABLE_SUFFIXES = {
    ".rs",
    ".md",
    ".toml",
    ".json",
    ".json5",
    ".yaml",
    ".yml",
    ".txt",
    ".sh",
    ".bash",
    ".zsh",
    ".fish",
    ".js",
    ".jsx",
    ".ts",
    ".tsx",
    ".sql",
    ".snap",
}

RUST_KEYWORDS = {
    "as",
    "async",
    "await",
    "break",
    "const",
    "continue",
    "crate",
    "else",
    "enum",
    "false",
    "fn",
    "for",
    "if",
    "impl",
    "in",
    "let",
    "loop",
    "match",
    "mod",
    "move",
    "mut",
    "pub",
    "ref",
    "return",
    "self",
    "Self",
    "static",
    "struct",
    "super",
    "trait",
    "true",
    "type",
    "unsafe",
    "use",
    "where",
    "while",
}

COMMON_CALL_TOKENS = {
    "as_ref",
    "clone",
    "collect",
    "default",
    "expect",
    "from",
    "into",
    "is_empty",
    "is_none",
    "is_some",
    "map",
    "new",
    "ok",
    "push",
    "to_owned",
    "to_string",
    "unwrap",
}

VISIBILITY = r"(?:pub(?:\([^)]*\))?\s+)?"
FN_RE = re.compile(
    r"^\s*"
    + VISIBILITY
    + r"(?:async\s+)?(?:unsafe\s+)?(?:const\s+)?"
    + r"(?:extern\s+\"[^\"]+\"\s+)?"
    + r"fn\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\b"
)
IMPL_RE = re.compile(r"^\s*impl(?:\s*<[^>{}]*>)?\s+(?P<body>[^{;]+)")
CONST_RE = re.compile(
    r"^\s*" + VISIBILITY + r"const\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\b"
)
STATIC_RE = re.compile(
    r"^\s*" + VISIBILITY + r"static\s+(?:mut\s+)?(?P<name>[A-Za-z_][A-Za-z0-9_]*)\b"
)
TYPE_ALIAS_RE = re.compile(
    r"^\s*" + VISIBILITY + r"type\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\b"
)
STRUCT_RE = re.compile(
    r"^\s*" + VISIBILITY + r"struct\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\b"
)
ENUM_RE = re.compile(
    r"^\s*" + VISIBILITY + r"enum\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\b"
)
TRAIT_RE = re.compile(
    r"^\s*" + VISIBILITY + r"(?:unsafe\s+)?trait\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\b"
)
MOD_RE = re.compile(r"^\s*" + VISIBILITY + r"mod\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\b")
USE_RE = re.compile(r"^\s*(?P<vis>pub(?:\([^)]*\))?\s+)?use\s+(?P<path>[^;]+);")
MACRO_RULES_RE = re.compile(r"^\s*macro_rules!\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)\b")
FIELD_RE = re.compile(r"^\s*" + VISIBILITY + r"(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*:")
CALL_RE = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*(?:::<[^>]+>)?\s*\(")
ASSOCIATED_RE = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)+)\b")
TYPE_RE = re.compile(r"\b([A-Z][A-Za-z0-9_]{2,})\b")
MACRO_RE = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)!\s*")
DYN_TRAIT_RE = re.compile(r"\bdyn\s+([A-Za-z_][A-Za-z0-9_:]*)")
CFG_ATTR_RE = re.compile(r"#\[cfg\(([^\]]+)\)\]")
CFG_MACRO_RE = re.compile(r"\bcfg!\(([^)]+)\)")
T = TypeVar("T")


@dataclasses.dataclass(frozen=True)
class ImplBlock:
    owner_type: str
    trait_name: str | None
    path: Path
    start_line: int
    end_line: int


@dataclasses.dataclass(frozen=True)
class ItemBlock:
    kind: str
    name: str
    path: Path
    start_line: int
    end_line: int


@dataclasses.dataclass(frozen=True)
class Declaration:
    path: Path
    relpath: str
    line: int
    end_line: int
    name: str
    owner_type: str | None
    qualified_name: str
    signature: str
    kind: str = "function"
    module_path: tuple[str, ...] = ()
    owner_trait: str | None = None
    parent_kind: str | None = None
    attr_start_line: int | None = None
    attrs: tuple[str, ...] = ()
    cfgs: tuple[str, ...] = ()
    resolution_note: str | None = None


@dataclasses.dataclass
class Hit:
    path: Path
    relpath: str
    line: int
    text: str
    terms: set[str]
    category: str = ""

    def key(self) -> tuple[str, int, str]:
        return (self.relpath, self.line, self.text)


@dataclasses.dataclass(frozen=True)
class Overlay:
    relpath: str
    line: int
    text: str
    reasons: tuple[str, ...]


@dataclasses.dataclass(frozen=True)
class HighRiskUnknown:
    kind: str
    relpath: str
    line: int
    evidence: str
    follow_up_commands: tuple[str, ...] = ()


@dataclasses.dataclass
class SearchResult:
    backend: str
    hits: list[Hit]
    warnings: list[str]
    high_risk_unknowns: list[HighRiskUnknown]


@dataclasses.dataclass(frozen=True)
class ReportWriteResult:
    path: str | None
    sha256: str | None
    bytes_written: int | None


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Report a conservative Rust blast radius for a function or symbol."
    )
    target = parser.add_argument_group("target")
    target.add_argument(
        "--symbol", help="Symbol or qualified symbol, e.g. Type::method."
    )
    target.add_argument(
        "--file",
        help="Workspace-relative or absolute Rust file containing the target line.",
    )
    target.add_argument(
        "--line", type=positive_int, help="1-based target line for --file."
    )
    parser.add_argument(
        "--workspace", default=".", help="Workspace root. Defaults to cwd."
    )
    parser.add_argument(
        "--json", action="store_true", help="Emit machine-readable JSON."
    )
    parser.add_argument(
        "--summary",
        action="store_true",
        help="Emit a compact stdout summary. Pair with --write-report for uncapped evidence without stdout truncation.",
    )
    parser.add_argument(
        "--write-report",
        help="Write the full uncapped/capped JSON report to this path and print only a receipt/summary unless --json is used without --summary.",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="Exit 3 when high-risk unknowns are present.",
    )
    parser.add_argument(
        "--max-hits",
        type=positive_int,
        default=None,
        help="Optional explicit display cap per bucket. Defaults to exhaustive output.",
    )
    parser.add_argument(
        "--include",
        action="append",
        default=[],
        help="Repeatable regex matched against workspace-relative paths to include.",
    )
    parser.add_argument(
        "--exclude",
        action="append",
        default=[],
        help="Repeatable regex matched against workspace-relative paths to exclude.",
    )
    return parser.parse_args(argv)


def positive_int(raw_value: str) -> int:
    try:
        value = int(raw_value)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("must be an integer") from exc
    if value <= 0:
        raise argparse.ArgumentTypeError("must be > 0")
    return value


def main(argv: Sequence[str]) -> int:
    args = parse_args(argv)
    try:
        config = build_config(args)
        report, exit_code = build_report(config)
        write_result = write_report_if_requested(config, report)
    except UserFacingError as exc:
        print(f"[error] {exc}", file=sys.stderr)
        return exc.exit_code

    if args.json and not args.summary and not args.write_report:
        print(json.dumps(report, indent=2, ensure_ascii=False, sort_keys=True))
    elif args.json:
        print(
            json.dumps(
                summary_to_json(report, write_result),
                indent=2,
                ensure_ascii=False,
                sort_keys=True,
            )
        )
    elif args.summary or args.write_report:
        print_human_summary(report, write_result)
    else:
        print_human_report(report)

    for warning in report["warnings"]:
        print(f"[warning] {warning}", file=sys.stderr)
    if exit_code == 3:
        print("[error] strict mode found high-risk unknowns", file=sys.stderr)
    return exit_code


@dataclasses.dataclass(frozen=True)
class Config:
    workspace: Path
    symbol: str | None
    target_file: Path | None
    target_line: int | None
    max_hits: int | None
    strict: bool
    include_patterns: tuple[re.Pattern[str], ...]
    exclude_patterns: tuple[re.Pattern[str], ...]
    summary: bool
    write_report: Path | None


class UserFacingError(Exception):
    def __init__(self, message: str, exit_code: int = 1) -> None:
        super().__init__(message)
        self.exit_code = exit_code


def build_config(args: argparse.Namespace) -> Config:
    workspace = Path(args.workspace).expanduser().resolve()
    if not workspace.exists() or not workspace.is_dir():
        raise UserFacingError(f"workspace is not a directory: {workspace}", 1)

    has_symbol = bool(args.symbol)
    has_file_line = bool(args.file) or bool(args.line)
    if has_symbol == has_file_line:
        raise UserFacingError(
            "provide exactly one target form: --symbol OR --file with --line", 1
        )
    if has_file_line and (not args.file or not args.line):
        raise UserFacingError("--file and --line must be provided together", 1)

    target_file = None
    if args.file:
        raw_file = Path(args.file).expanduser()
        target_file = raw_file if raw_file.is_absolute() else workspace / raw_file
        target_file = target_file.resolve()
        if not target_file.exists() or not target_file.is_file():
            raise UserFacingError(f"target file is not readable: {target_file}", 1)
        try:
            target_file.relative_to(workspace)
        except ValueError as exc:
            raise UserFacingError(
                f"target file is outside workspace: {target_file}", 1
            ) from exc

    write_report = None
    if args.write_report:
        raw_report = Path(args.write_report).expanduser()
        write_report = (
            raw_report if raw_report.is_absolute() else workspace / raw_report
        )
        write_report = write_report.resolve()

    include_patterns = compile_path_patterns(args.include, "--include")
    exclude_patterns = compile_path_patterns(args.exclude, "--exclude")
    return Config(
        workspace=workspace,
        symbol=args.symbol,
        target_file=target_file,
        target_line=args.line,
        max_hits=args.max_hits,
        strict=args.strict,
        include_patterns=include_patterns,
        exclude_patterns=exclude_patterns,
        summary=args.summary,
        write_report=write_report,
    )


def compile_path_patterns(
    raw_patterns: Sequence[str], flag_name: str
) -> tuple[re.Pattern[str], ...]:
    compiled: list[re.Pattern[str]] = []
    for raw_pattern in raw_patterns:
        try:
            compiled.append(re.compile(raw_pattern))
        except re.error as exc:
            raise UserFacingError(
                f"invalid {flag_name} regex {raw_pattern!r}: {exc}", 1
            ) from exc
    return tuple(compiled)


def build_report(config: Config) -> tuple[dict[str, object], int]:
    rust_files = list(iter_target_index_files(config.workspace, config))
    declarations = parse_declarations(config.workspace, rust_files)
    target_status, target_declarations, candidates = resolve_target(
        config, declarations
    )
    if target_status == "unresolved":
        report = base_report(config, "unresolved", "none")
        add_target_candidates(report, candidates, config.max_hits, config)
        report["suggested_next_commands"] = unresolved_next_commands(config, candidates)
        report["warnings"].append("target could not be resolved")
        if candidates:
            report["warnings"].append(
                "unresolved target has same-leaf candidate declarations; requested owner may be stale"
            )
        report["warnings"].extend(
            target_candidate_capping_warnings(candidates, config.max_hits)
        )
        return report, 2
    if target_status == "ambiguous":
        report = base_report(config, "ambiguous", "none")
        add_target_candidates(report, candidates, config.max_hits, config)
        report["suggested_next_commands"] = target_candidate_commands(
            config, candidates
        )
        report["warnings"].append(
            "target is ambiguous; use one of the suggested --file --line commands"
        )
        report["warnings"].extend(
            target_candidate_capping_warnings(candidates, config.max_hits)
        )
        return report, 2

    target_declaration = target_declarations[0]
    target_resolution = target_to_json(config, target_declaration)
    search_patterns = build_search_patterns(config, target_declaration)
    search_result = (
        search_workspace(config, search_patterns)
        if search_patterns
        else empty_search_pattern_result(target_declaration)
    )
    inbound_buckets = classify_hits(config, target_declaration, search_result.hits)
    body_lines = read_declaration_body(target_declaration)
    outbound_tokens = extract_outbound_tokens(body_lines, target_declaration)
    high_risk_unknowns = list(search_result.high_risk_unknowns)
    high_risk_unknowns.extend(detect_high_risk_unknowns(target_declaration, body_lines))
    atlas_overlays = collect_atlas_overlays(config, target_declaration)
    warnings = list(search_result.warnings)
    warnings.extend(
        bucket_capping_warnings(inbound_buckets, atlas_overlays, config.max_hits)
    )

    report = base_report(config, "resolved", search_result.backend)
    report["target"] = target_resolution
    report["owner_declarations"] = [declaration_to_json(target_declaration, config)]
    report["inbound_references"] = {
        bucket_name: hit_bucket_to_json(hits, config.max_hits)
        for bucket_name, hits in inbound_buckets.items()
    }
    report["outbound_tokens"] = outbound_tokens
    report["atlas_candidate_overlays"] = overlay_bucket_to_json(
        atlas_overlays, config.max_hits
    )
    report["high_risk_unknowns"] = [
        dataclasses.asdict(item) for item in high_risk_unknowns
    ]
    report["suggested_next_commands"] = suggested_next_commands(
        config, target_declaration, high_risk_unknowns
    )
    report["warnings"] = warnings
    report["truncation"] = {
        "max_hits_per_bucket": config.max_hits,
        "buckets_capped": [
            name
            for name, hits in inbound_buckets.items()
            if is_capped(hits, config.max_hits)
        ],
        "atlas_overlays_capped": is_capped(atlas_overlays, config.max_hits),
    }

    exit_code = 3 if config.strict and high_risk_unknowns else 0
    return report, exit_code


def base_report(config: Config, status: str, backend: str) -> dict[str, object]:
    return {
        "status": status,
        "workspace": str(config.workspace),
        "backend": backend,
        "strict": config.strict,
        "report": {
            "schema_version": 2,
            "uncapped": config.max_hits is None,
            "stdout_mode": "summary"
            if config.summary or config.write_report
            else "full",
        },
        "target": {
            "requested_symbol": config.symbol,
            "requested_file": relpath(config.workspace, config.target_file)
            if config.target_file
            else None,
            "requested_line": config.target_line,
        },
        "owner_declarations": [],
        "inbound_references": {},
        "outbound_tokens": {},
        "atlas_candidate_overlays": {"total": 0, "items": []},
        "high_risk_unknowns": [],
        "suggested_next_commands": [],
        "truncation": {"max_hits_per_bucket": config.max_hits, "buckets_capped": []},
        "warnings": [],
    }


def add_target_candidates(
    report: dict[str, object],
    candidates: Sequence[Declaration],
    max_hits: int | None,
    config: Config,
) -> None:
    target = report["target"]
    if not isinstance(target, dict):
        raise TypeError("report target must be a dict")
    target["candidate_total"] = len(candidates)
    target["candidate_shown"] = shown_count(candidates, max_hits)
    target["candidates"] = [
        declaration_to_json(candidate, config)
        for candidate in limit_items(candidates, max_hits)
    ]


def iter_target_index_files(workspace: Path, config: Config) -> Iterable[Path]:
    yielded: set[Path] = set()
    if config.target_file and config.target_file.suffix == ".rs":
        yielded.add(config.target_file)
        yield config.target_file
    for path in iter_searchable_files(
        workspace, config, rust_only=True, apply_include=False
    ):
        if path not in yielded:
            yield path


def iter_searchable_files(
    workspace: Path,
    config: Config,
    *,
    rust_only: bool = False,
    apply_include: bool = True,
) -> Iterable[Path]:
    for root, dir_names, file_names in os.walk(workspace):
        dir_names[:] = [
            dir_name
            for dir_name in dir_names
            if dir_name not in SKIPPED_DIR_NAMES
            and not should_prune_dir(workspace, Path(root) / dir_name, config)
        ]
        for file_name in file_names:
            path = Path(root) / file_name
            if should_skip_file(workspace, path, config, apply_include=apply_include):
                continue
            if rust_only:
                if path.suffix == ".rs":
                    yield path
                continue
            if is_searchable_file(path):
                yield path


def should_prune_dir(workspace: Path, path: Path, config: Config) -> bool:
    path_text = relpath(workspace, path)
    return any(pattern.search(path_text) for pattern in config.exclude_patterns)


def should_skip_file(
    workspace: Path, path: Path, config: Config, *, apply_include: bool = True
) -> bool:
    path_text = relpath(workspace, path)
    if any(pattern.search(path_text) for pattern in config.exclude_patterns):
        return True
    if (
        apply_include
        and config.include_patterns
        and not any(pattern.search(path_text) for pattern in config.include_patterns)
    ):
        return True
    return False


def is_searchable_file(path: Path) -> bool:
    if path.name in {"AGENTS.md", "README", "NOTICE", "LICENSE"}:
        return True
    if path.suffix not in SEARCHABLE_SUFFIXES:
        return False
    try:
        if path.stat().st_size > 8 * 1024 * 1024:
            return False
    except OSError:
        return False
    return True


def parse_declarations(
    workspace: Path, rust_files: Sequence[Path]
) -> list[Declaration]:
    declarations: list[Declaration] = []
    for rust_file in rust_files:
        try:
            lines = rust_file.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            continue
        declarations.extend(parse_file_declarations(workspace, rust_file, lines))
    return sorted(
        declarations,
        key=lambda item: (
            item.relpath,
            item.line,
            item.end_line,
            item.kind,
            item.qualified_name,
        ),
    )


def parse_file_declarations(
    workspace: Path, rust_file: Path, lines: Sequence[str]
) -> list[Declaration]:
    rel = relpath(workspace, rust_file)
    module_path = tuple(rust_module_parts(rel))
    impl_blocks = parse_impl_blocks(rust_file, lines)
    trait_blocks = parse_named_blocks(rust_file, lines, TRAIT_RE, "trait")
    struct_blocks = parse_named_blocks(rust_file, lines, STRUCT_RE, "struct")
    declarations: list[Declaration] = []
    seen: set[tuple[str, str, int, str | None]] = set()

    def add(
        *,
        kind: str,
        name: str,
        line_index: int,
        end_line: int,
        signature: str | None = None,
        owner_type: str | None = None,
        owner_trait: str | None = None,
        parent_kind: str | None = None,
        resolution_note: str | None = None,
    ) -> None:
        key = (kind, name, line_index, owner_type)
        if key in seen:
            return
        seen.add(key)
        attr_start, attrs, cfgs = leading_attrs(lines, line_index)
        qualified_name = f"{owner_type}::{name}" if owner_type else name
        declarations.append(
            Declaration(
                path=rust_file,
                relpath=rel,
                line=line_index,
                end_line=end_line,
                name=name,
                owner_type=owner_type,
                qualified_name=qualified_name,
                signature=signature
                if signature is not None
                else lines[line_index - 1].strip(),
                kind=kind,
                module_path=module_path,
                owner_trait=owner_trait,
                parent_kind=parent_kind,
                attr_start_line=attr_start,
                attrs=attrs,
                cfgs=cfgs,
                resolution_note=resolution_note,
            )
        )

    for impl_block in impl_blocks:
        add(
            kind="impl_block",
            name=impl_block.owner_type,
            line_index=impl_block.start_line,
            end_line=impl_block.end_line,
            signature=lines[impl_block.start_line - 1].strip(),
            owner_type=impl_block.owner_type,
            owner_trait=impl_block.trait_name,
        )

    for block in trait_blocks:
        add(
            kind="trait",
            name=block.name,
            line_index=block.start_line,
            end_line=block.end_line,
            signature=lines[block.start_line - 1].strip(),
        )
    for block in struct_blocks:
        add(
            kind="struct",
            name=block.name,
            line_index=block.start_line,
            end_line=block.end_line,
            signature=lines[block.start_line - 1].strip(),
        )

    for line_index, line in enumerate(lines, start=1):
        stripped = strip_line_comment(line).strip()
        if not stripped:
            continue
        impl_block = innermost_impl_block(impl_blocks, line_index)
        trait_block = innermost_item_block(trait_blocks, line_index)
        struct_block = innermost_item_block(struct_blocks, line_index)

        fn_match = FN_RE.match(line)
        if fn_match:
            name = fn_match.group("name")
            if impl_block:
                add(
                    kind="method",
                    name=name,
                    line_index=line_index,
                    end_line=find_block_end(lines, line_index),
                    owner_type=impl_block.owner_type,
                    owner_trait=impl_block.trait_name,
                    parent_kind="impl",
                )
            elif trait_block:
                add(
                    kind="trait_method",
                    name=name,
                    line_index=line_index,
                    end_line=find_block_end(lines, line_index),
                    owner_type=trait_block.name,
                    parent_kind="trait",
                )
            else:
                add(
                    kind="function",
                    name=name,
                    line_index=line_index,
                    end_line=find_block_end(lines, line_index),
                )
            continue

        if (
            struct_block
            and struct_block.start_line < line_index < struct_block.end_line
        ):
            field_match = FIELD_RE.match(line)
            if field_match and "(" not in stripped.split(":", 1)[0]:
                add(
                    kind="field",
                    name=field_match.group("name"),
                    line_index=line_index,
                    end_line=find_field_end(lines, line_index),
                    owner_type=struct_block.name,
                    parent_kind="struct",
                )
                continue

        for regex, kind in (
            (CONST_RE, "const"),
            (STATIC_RE, "static"),
            (TYPE_ALIAS_RE, "type_alias"),
            (ENUM_RE, "enum"),
            (MOD_RE, "mod"),
            (MACRO_RULES_RE, "macro_rules"),
        ):
            match = regex.match(line)
            if match:
                add(
                    kind=kind,
                    name=match.group("name"),
                    line_index=line_index,
                    end_line=find_block_end(lines, line_index),
                )
                break
        else:
            use_match = USE_RE.match(line)
            if use_match:
                path_text = use_match.group("path").strip()
                name = imported_item_name(path_text)
                kind = "reexport" if use_match.group("vis") else "use"
                add(
                    kind=kind,
                    name=name,
                    line_index=line_index,
                    end_line=line_index,
                    signature=line.strip(),
                )
    return declarations


def parse_named_blocks(
    path: Path, lines: Sequence[str], regex: re.Pattern[str], kind: str
) -> list[ItemBlock]:
    blocks: list[ItemBlock] = []
    for line_index, line in enumerate(lines, start=1):
        match = regex.match(line)
        if not match:
            continue
        blocks.append(
            ItemBlock(
                kind=kind,
                name=match.group("name"),
                path=path,
                start_line=line_index,
                end_line=find_block_end(lines, line_index),
            )
        )
    return blocks


def parse_impl_blocks(path: Path, lines: Sequence[str]) -> list[ImplBlock]:
    impl_blocks: list[ImplBlock] = []
    for line_index, line in enumerate(lines, start=1):
        match = IMPL_RE.match(line)
        if not match:
            continue
        owner_type, trait_name = parse_impl_owner(match.group("body"))
        if not owner_type:
            continue
        impl_blocks.append(
            ImplBlock(
                owner_type=owner_type,
                trait_name=trait_name,
                path=path,
                start_line=line_index,
                end_line=find_block_end(lines, line_index),
            )
        )
    return impl_blocks


def parse_impl_owner(raw_body: str) -> tuple[str | None, str | None]:
    cleaned = raw_body.split("where", 1)[0].strip()
    if " for " in cleaned:
        trait_name, owner = cleaned.rsplit(" for ", 1)
        return simplify_type_name(owner), simplify_type_name(trait_name)
    return simplify_type_name(cleaned), None


def simplify_type_name(raw_type: str) -> str | None:
    stripped = raw_type.strip().strip("{").strip()
    stripped = re.sub(r"<.*", "", stripped)
    stripped = stripped.strip("& ").strip()
    match = re.search(r"([A-Za-z_][A-Za-z0-9_]*)\s*$", stripped)
    return match.group(1) if match else None


def innermost_impl_block(
    impl_blocks: Sequence[ImplBlock], line_number: int
) -> ImplBlock | None:
    candidates = [
        impl_block
        for impl_block in impl_blocks
        if impl_block.start_line < line_number <= impl_block.end_line
    ]
    if not candidates:
        return None
    return max(candidates, key=lambda item: item.start_line)


def innermost_item_block(
    blocks: Sequence[ItemBlock], line_number: int
) -> ItemBlock | None:
    candidates = [
        block for block in blocks if block.start_line < line_number <= block.end_line
    ]
    if not candidates:
        return None
    return max(candidates, key=lambda item: item.start_line)


def find_block_end(lines: Sequence[str], start_line: int) -> int:
    saw_open = False
    brace_depth = 0
    for line_index in range(start_line, len(lines) + 1):
        line = strip_line_comment(lines[line_index - 1])
        if "{" in line:
            saw_open = True
        brace_depth += line.count("{") - line.count("}")
        if saw_open and brace_depth <= 0:
            return line_index
        if not saw_open and ";" in line:
            return line_index
    return start_line


def find_field_end(lines: Sequence[str], start_line: int) -> int:
    for line_index in range(start_line, len(lines) + 1):
        line = strip_line_comment(lines[line_index - 1])
        if "," in line or "}" in line:
            return line_index
    return start_line


def strip_line_comment(line: str) -> str:
    in_string = False
    escaped = False
    for index in range(len(line) - 1):
        char = line[index]
        if char == "\\" and in_string:
            escaped = not escaped
            continue
        if char == '"' and not escaped:
            in_string = not in_string
        escaped = False
        if not in_string and line[index : index + 2] == "//":
            return line[:index]
    return line


def leading_attrs(
    lines: Sequence[str], line_index: int
) -> tuple[int, tuple[str, ...], tuple[str, ...]]:
    attrs: list[str] = []
    current = line_index - 1
    while current > 0:
        stripped = lines[current - 1].strip()
        if not stripped:
            current -= 1
            continue
        if (
            stripped.startswith("#")
            or stripped.startswith("///")
            or stripped.startswith("//!")
        ):
            attrs.insert(0, stripped)
            current -= 1
            continue
        break
    attr_start = current + 1 if attrs else line_index
    cfgs = tuple(
        cfg_match.group(1) for attr in attrs for cfg_match in CFG_ATTR_RE.finditer(attr)
    )
    return attr_start, tuple(attrs), cfgs


def imported_item_name(path_text: str) -> str:
    alias_match = re.search(r"\bas\s+([A-Za-z_][A-Za-z0-9_]*)\s*$", path_text)
    if alias_match:
        return alias_match.group(1)
    cleaned = path_text.strip().rstrip(",")
    if cleaned.endswith("}") and "{" in cleaned:
        inner = cleaned.rsplit("{", 1)[1].rstrip("}").strip()
        if "," in inner:
            return "{" + inner + "}"
        return imported_item_name(inner)
    match = re.search(r"([A-Za-z_][A-Za-z0-9_]*)\s*$", cleaned)
    return match.group(1) if match else cleaned


def resolve_target(
    config: Config, declarations: Sequence[Declaration]
) -> tuple[str, list[Declaration], list[Declaration]]:
    if config.target_file and config.target_line:
        matching = [
            declaration
            for declaration in declarations
            if declaration.path == config.target_file
            and declaration_contains_line(declaration, config.target_line)
        ]
        if not matching:
            return "resolved", [file_module_declaration(config)], []
        return (
            "resolved",
            [min(matching, key=declaration_span_score)],
            matching,
        )

    assert config.symbol is not None
    symbol = config.symbol.strip()
    matches = symbol_matches(symbol, declarations)
    if not matches:
        leaf = symbol.rsplit("::", 1)[-1]
        candidates = [
            declaration for declaration in declarations if declaration.name == leaf
        ]
        return "unresolved", [], sort_candidates(candidates)
    primary = [
        candidate for candidate in matches if candidate.kind not in {"use", "reexport"}
    ]
    if len(primary) == 1:
        return "resolved", primary, matches
    chosen_matches = primary if primary else matches
    if len(chosen_matches) > 1:
        return "ambiguous", [], sort_candidates(chosen_matches)
    return "resolved", chosen_matches, matches


def declaration_contains_line(declaration: Declaration, line_number: int) -> bool:
    start_line = declaration.attr_start_line or declaration.line
    return start_line <= line_number <= declaration.end_line


def declaration_span_score(declaration: Declaration) -> tuple[int, int]:
    start_line = declaration.attr_start_line or declaration.line
    return (declaration.end_line - start_line, kind_resolution_rank(declaration.kind))


def kind_resolution_rank(kind: str) -> int:
    ranks = {
        "field": 0,
        "const": 1,
        "static": 1,
        "type_alias": 1,
        "function": 2,
        "method": 2,
        "trait_method": 2,
        "use": 3,
        "reexport": 3,
        "mod": 4,
        "struct": 5,
        "enum": 5,
        "trait": 5,
        "impl_block": 6,
        "file_module": 99,
    }
    return ranks.get(kind, 50)


def sort_candidates(candidates: Sequence[Declaration]) -> list[Declaration]:
    return sorted(candidates, key=lambda item: (item.relpath, item.line, item.kind))


def symbol_matches(
    symbol: str, declarations: Sequence[Declaration]
) -> list[Declaration]:
    if "::" in symbol:
        owner, name = symbol.rsplit("::", 1)
        owner_leaf = owner.rsplit("::", 1)[-1]
        exact = [
            declaration
            for declaration in declarations
            if declaration.qualified_name == symbol
        ]
        if exact:
            return sort_candidates(exact)
        return sort_candidates(
            [
                declaration
                for declaration in declarations
                if declaration.name == name
                and (
                    declaration.owner_type == owner_leaf
                    or declaration.owner_trait == owner_leaf
                    or "::".join((*declaration.module_path, declaration.name)) == symbol
                )
            ]
        )
    return sort_candidates(
        [
            declaration
            for declaration in declarations
            if declaration.name == symbol or declaration.qualified_name == symbol
        ]
    )


def file_module_declaration(config: Config) -> Declaration:
    assert config.target_file is not None
    rel = relpath(config.workspace, config.target_file)
    module_path = tuple(rust_module_parts(rel))
    try:
        lines = config.target_file.read_text(
            encoding="utf-8", errors="replace"
        ).splitlines()
    except OSError:
        lines = []
    line = min(config.target_line or 1, max(len(lines), 1))
    name = "::".join(module_path) if module_path else Path(rel).stem
    return Declaration(
        path=config.target_file,
        relpath=rel,
        line=line,
        end_line=line,
        name=name,
        owner_type=None,
        qualified_name=name,
        signature=lines[line - 1].strip()
        if lines and 0 <= line - 1 < len(lines)
        else rel,
        kind="file_module",
        module_path=module_path,
        resolution_note="requested line is not a recognized Rust item; resolved to file/module scope",
    )


def build_search_patterns(
    config: Config, declaration: Declaration
) -> list[tuple[str, str]]:
    patterns: list[tuple[str, str]] = []
    if config.symbol:
        patterns.append((f"requested symbol {config.symbol}", re.escape(config.symbol)))

    if declaration.kind in {"function", "method", "trait_method"}:
        add_callable_patterns(patterns, declaration)
    elif declaration.kind in {"const", "static"}:
        add_word_and_path_patterns(patterns, declaration, "constant/static")
    elif declaration.kind in {"type_alias", "struct", "enum", "trait"}:
        add_type_patterns(patterns, declaration)
    elif declaration.kind == "field":
        add_field_patterns(patterns, declaration)
    elif declaration.kind in {"use", "reexport"}:
        if len(declaration.name) >= 4 and not declaration.name.startswith("{"):
            patterns.append(
                (
                    f"imported item {declaration.name}",
                    rf"\b{re.escape(declaration.name)}\b",
                )
            )
        path_terms = associated_path_terms(declaration.signature)
        for term in path_terms:
            patterns.append((f"import path {term}", re.escape(term)))
    elif declaration.kind == "mod":
        patterns.append(
            (f"module {declaration.name}", rf"\bmod\s+{re.escape(declaration.name)}\b")
        )
        if len(declaration.name) >= 4:
            patterns.append(
                (
                    f"module path {declaration.name}",
                    rf"\b{re.escape(declaration.name)}\b",
                )
            )
    elif declaration.kind == "impl_block" and declaration.owner_type:
        patterns.append(
            (
                f"owner type {declaration.owner_type}",
                rf"\b{re.escape(declaration.owner_type)}\b",
            )
        )
        if declaration.owner_trait:
            patterns.append(
                (
                    f"impl trait {declaration.owner_trait}",
                    rf"\b{re.escape(declaration.owner_trait)}\b",
                )
            )
    elif declaration.kind == "file_module":
        for part in declaration.module_path:
            if len(part) >= 4:
                patterns.append((f"module part {part}", rf"\b{re.escape(part)}\b"))
    return dedupe_patterns(patterns)


def add_callable_patterns(
    patterns: list[tuple[str, str]], declaration: Declaration
) -> None:
    if declaration.owner_type:
        patterns.append(
            (
                f"qualified call {declaration.owner_type}::{declaration.name}",
                rf"\b{re.escape(declaration.owner_type)}\s*::\s*{re.escape(declaration.name)}\b",
            )
        )
        patterns.append(
            (
                f"method call .{declaration.name}(",
                rf"\.\s*{re.escape(declaration.name)}\s*(?:::<[^>]+>)?\s*\(",
            )
        )
        patterns.append(
            (
                f"owner type {declaration.owner_type}",
                rf"\b{re.escape(declaration.owner_type)}\b",
            )
        )
    if len(declaration.name) >= 4:
        patterns.append(
            (
                f"direct call {declaration.name}(",
                rf"\b{re.escape(declaration.name)}\s*(?:::<[^>]+>)?\s*\(",
            )
        )
    if declaration.owner_type is None:
        for module_parts in free_function_module_paths(declaration):
            module_path = "::".join((*module_parts, declaration.name))
            patterns.append(
                (
                    f"module-qualified call {module_path}(",
                    rust_path_call_pattern((*module_parts, declaration.name)),
                )
            )


def add_word_and_path_patterns(
    patterns: list[tuple[str, str]], declaration: Declaration, label: str
) -> None:
    if len(declaration.name) >= 3:
        patterns.append(
            (f"{label} {declaration.name}", rf"\b{re.escape(declaration.name)}\b")
        )
    for module_parts in free_function_module_paths(declaration):
        module_path = "::".join((*module_parts, declaration.name))
        patterns.append(
            (
                f"module-qualified item {module_path}",
                rust_path_word_pattern((*module_parts, declaration.name)),
            )
        )
    if declaration.owner_type:
        patterns.append(
            (
                f"qualified item {declaration.owner_type}::{declaration.name}",
                rf"\b{re.escape(declaration.owner_type)}\s*::\s*{re.escape(declaration.name)}\b",
            )
        )


def add_type_patterns(
    patterns: list[tuple[str, str]], declaration: Declaration
) -> None:
    if len(declaration.name) >= 3:
        patterns.append(
            (f"type {declaration.name}", rf"\b{re.escape(declaration.name)}\b")
        )
        patterns.append(
            (f"dyn {declaration.name}", rf"\bdyn\s+{re.escape(declaration.name)}\b")
        )
        patterns.append(
            (
                f"impl for {declaration.name}",
                rf"\bimpl\b[^\n]*\b{re.escape(declaration.name)}\b",
            )
        )


def add_field_patterns(
    patterns: list[tuple[str, str]], declaration: Declaration
) -> None:
    patterns.append(
        (f"field access .{declaration.name}", rf"\.\s*{re.escape(declaration.name)}\b")
    )
    patterns.append(
        (f"field literal {declaration.name}:", rf"\b{re.escape(declaration.name)}\s*:")
    )
    if declaration.owner_type:
        patterns.append(
            (
                f"owner type {declaration.owner_type}",
                rf"\b{re.escape(declaration.owner_type)}\b",
            )
        )
    if len(declaration.name) >= 8:
        patterns.append(
            (f"field word {declaration.name}", rf"\b{re.escape(declaration.name)}\b")
        )


def associated_path_terms(signature: str) -> list[str]:
    return sorted(set(ASSOCIATED_RE.findall(signature)))


def empty_search_pattern_result(declaration: Declaration) -> SearchResult:
    warning = "no conservative inbound search patterns built; report is incomplete"
    return SearchResult(
        backend="none",
        hits=[],
        warnings=[warning],
        high_risk_unknowns=[
            HighRiskUnknown(
                "no-inbound-search-pattern",
                declaration.relpath,
                declaration.line,
                warning,
            )
        ],
    )


def free_function_module_paths(declaration: Declaration) -> list[tuple[str, ...]]:
    module_parts = list(declaration.module_path) or rust_module_parts(
        declaration.relpath
    )
    if not module_parts:
        return []

    candidates = [tuple(module_parts)]
    last_module_part = (module_parts[-1],)
    if last_module_part not in candidates:
        candidates.append(last_module_part)
    return candidates


def rust_module_parts(relpath_text: str) -> list[str]:
    path_parts = Path(relpath_text).parts
    try:
        src_index = len(path_parts) - 1 - list(reversed(path_parts)).index("src")
        module_parts = list(path_parts[src_index + 1 : -1])
    except ValueError:
        module_parts = list(Path(relpath_text).parent.parts)

    stem = Path(relpath_text).stem
    if stem not in {"lib", "main", "mod"}:
        module_parts.append(stem)

    return [
        part for part in module_parts if re.match(r"^[A-Za-z_][A-Za-z0-9_]*$", part)
    ]


def rust_path_call_pattern(path_parts: Sequence[str]) -> str:
    separator = r"\s*::\s*"
    escaped_path = separator.join(re.escape(part) for part in path_parts)
    return rf"\b{escaped_path}\s*(?:::<[^>]+>)?\s*\("


def rust_path_word_pattern(path_parts: Sequence[str]) -> str:
    separator = r"\s*::\s*"
    escaped_path = separator.join(re.escape(part) for part in path_parts)
    return rf"\b{escaped_path}\b"


def dedupe_patterns(patterns: Sequence[tuple[str, str]]) -> list[tuple[str, str]]:
    seen: set[str] = set()
    deduped: list[tuple[str, str]] = []
    for label, pattern in patterns:
        if pattern in seen:
            continue
        seen.add(pattern)
        deduped.append((label, pattern))
    return deduped


def search_workspace(
    config: Config, patterns: Sequence[tuple[str, str]]
) -> SearchResult:
    rg_path = shutil.which("rg")
    if rg_path:
        try:
            return search_with_rg(config, patterns, rg_path)
        except (OSError, subprocess.SubprocessError) as exc:
            high_risk = [
                HighRiskUnknown(
                    kind="search-backend-failure",
                    relpath=".",
                    line=0,
                    evidence=f"rg failed and fallback will be attempted: {exc}",
                )
            ]
            fallback = search_with_python(config, patterns)
            fallback.high_risk_unknowns[:0] = high_risk if not fallback.hits else []
            fallback.warnings.insert(
                0, f"rg backend failed; used Python fallback: {exc}"
            )
            return fallback
    result = search_with_python(config, patterns)
    result.warnings.append("rg not found; used Python fallback")
    return result


def search_with_rg(
    config: Config, patterns: Sequence[tuple[str, str]], rg_path: str
) -> SearchResult:
    hits_by_key: dict[tuple[str, int, str], Hit] = {}
    compiled_patterns: list[tuple[str, re.Pattern[str]]] = []
    for label, pattern in patterns:
        try:
            compiled_patterns.append((label, re.compile(pattern)))
        except re.error as exc:
            return SearchResult(
                backend="rg",
                hits=[],
                warnings=[],
                high_risk_unknowns=[
                    HighRiskUnknown("invalid-search-pattern", ".", 0, f"{label}: {exc}")
                ],
            )

    combined_pattern = "|".join(f"(?:{pattern})" for _, pattern in patterns)
    command = [
        rg_path,
        "--hidden",
        "--line-number",
        "--no-heading",
        "--color=never",
        *rg_skipped_dir_globs(),
        "--regexp",
        combined_pattern,
        str(config.workspace),
    ]
    completed = subprocess.run(command, text=True, capture_output=True, check=False)
    if completed.returncode not in (0, 1):
        raise subprocess.SubprocessError(
            completed.stderr.strip() or f"rg exited {completed.returncode}"
        )
    for raw_line in completed.stdout.splitlines():
        hit = parse_rg_line_with_terms(config, raw_line, compiled_patterns)
        if not hit:
            continue
        existing = hits_by_key.get(hit.key())
        if existing:
            existing.terms.update(hit.terms)
        else:
            hits_by_key[hit.key()] = hit
    return SearchResult(
        backend="rg",
        hits=sorted(hits_by_key.values(), key=lambda item: (item.relpath, item.line)),
        warnings=[],
        high_risk_unknowns=[],
    )


def parse_rg_line_with_terms(
    config: Config,
    raw_line: str,
    compiled_patterns: Sequence[tuple[str, re.Pattern[str]]],
) -> Hit | None:
    parts = raw_line.split(":", 2)
    if len(parts) != 3:
        return None
    path_text, line_text, text = parts
    try:
        line_number = int(line_text)
    except ValueError:
        return None
    path = Path(path_text).resolve()
    if should_skip_file(config.workspace, path, config):
        return None
    terms = {label for label, pattern in compiled_patterns if pattern.search(text)}
    if not terms:
        return None
    return Hit(
        path=path,
        relpath=relpath(config.workspace, path),
        line=line_number,
        text=text.strip(),
        terms=terms,
    )


def rg_skipped_dir_globs() -> list[str]:
    globs: list[str] = []
    for dir_name in sorted(SKIPPED_DIR_NAMES):
        globs.extend(["--glob", f"!{dir_name}/**"])
        globs.extend(["--glob", f"!**/{dir_name}/**"])
    return globs


def parse_rg_line(config: Config, raw_line: str, label: str) -> Hit | None:
    parts = raw_line.split(":", 2)
    if len(parts) != 3:
        return None
    path_text, line_text, text = parts
    try:
        line_number = int(line_text)
    except ValueError:
        return None
    path = Path(path_text).resolve()
    if should_skip_file(config.workspace, path, config):
        return None
    return Hit(
        path=path,
        relpath=relpath(config.workspace, path),
        line=line_number,
        text=text.strip(),
        terms={label},
    )


def search_with_python(
    config: Config, patterns: Sequence[tuple[str, str]]
) -> SearchResult:
    compiled_patterns: list[tuple[str, re.Pattern[str]]] = []
    for label, pattern in patterns:
        try:
            compiled_patterns.append((label, re.compile(pattern)))
        except re.error as exc:
            return SearchResult(
                backend="python-fallback",
                hits=[],
                warnings=[],
                high_risk_unknowns=[
                    HighRiskUnknown("invalid-search-pattern", ".", 0, f"{label}: {exc}")
                ],
            )

    hits_by_key: dict[tuple[str, int, str], Hit] = {}
    high_risk_unknowns: list[HighRiskUnknown] = []
    for path in iter_searchable_files(config.workspace, config):
        try:
            with path.open("r", encoding="utf-8", errors="replace") as handle:
                for line_number, line in enumerate(handle, start=1):
                    for label, pattern in compiled_patterns:
                        if not pattern.search(line):
                            continue
                        hit = Hit(
                            path=path,
                            relpath=relpath(config.workspace, path),
                            line=line_number,
                            text=line.strip(),
                            terms={label},
                        )
                        existing = hits_by_key.get(hit.key())
                        if existing:
                            existing.terms.update(hit.terms)
                        else:
                            hits_by_key[hit.key()] = hit
        except OSError as exc:
            high_risk_unknowns.append(
                HighRiskUnknown(
                    "unreadable-search-file",
                    relpath(config.workspace, path),
                    0,
                    str(exc),
                )
            )
    return SearchResult(
        backend="python-fallback",
        hits=sorted(hits_by_key.values(), key=lambda item: (item.relpath, item.line)),
        warnings=[],
        high_risk_unknowns=high_risk_unknowns,
    )


def classify_hits(
    config: Config, declaration: Declaration, hits: Sequence[Hit]
) -> dict[str, list[Hit]]:
    buckets: dict[str, list[Hit]] = {
        "rust_production": [],
        "rust_tests_fixtures": [],
        "docs_schema_config_instructions": [],
        "other": [],
    }
    for hit in hits:
        if (
            hit.path == declaration.path
            and declaration.line <= hit.line <= declaration.end_line
        ):
            continue
        category = classify_hit_path(hit.relpath)
        hit.category = category
        buckets[category].append(hit)
    return buckets


def classify_hit_path(path_text: str) -> str:
    path = Path(path_text)
    parts = {part.lower() for part in path.parts}
    name = path.name.lower()
    if path.suffix == ".rs":
        if (
            "tests" in parts
            or "test" in parts
            or "fixtures" in parts
            or "fixture" in parts
            or "benches" in parts
            or "snapshots" in parts
            or "test" in name
            or "fixture" in name
            or "mock" in name
        ):
            return "rust_tests_fixtures"
        return "rust_production"
    if (
        path.name == "AGENTS.md"
        or "docs" in parts
        or ".sangoi" in parts
        or "schema" in name
        or path.suffix in {".md", ".toml", ".json", ".json5", ".yaml", ".yml"}
        or name.startswith("readme")
    ):
        return "docs_schema_config_instructions"
    return "other"


def read_declaration_body(declaration: Declaration) -> list[str]:
    try:
        lines = declaration.path.read_text(
            encoding="utf-8", errors="replace"
        ).splitlines()
    except OSError:
        return []
    return lines[declaration.line - 1 : declaration.end_line]


def extract_outbound_tokens(
    body_lines: Sequence[str], declaration: Declaration
) -> dict[str, object]:
    body_text = "\n".join(body_lines)
    calls = sorted(
        {
            token
            for token in CALL_RE.findall(body_text)
            if token not in RUST_KEYWORDS
            and token not in COMMON_CALL_TOKENS
            and token != declaration.name
        }
    )
    associated = sorted(set(ASSOCIATED_RE.findall(body_text)))
    types = sorted(
        {
            token
            for token in TYPE_RE.findall(body_text)
            if token not in {declaration.owner_type, "Self"}
        }
    )
    macros = sorted({token for token in MACRO_RE.findall(body_text) if token != "cfg"})
    return {
        "calls": calls,
        "associated_paths": associated,
        "types": types,
        "macros": macros,
        "totals": {
            "calls": len(calls),
            "associated_paths": len(associated),
            "types": len(types),
            "macros": len(macros),
        },
    }


def detect_high_risk_unknowns(
    declaration: Declaration, body_lines: Sequence[str]
) -> list[HighRiskUnknown]:
    unknowns: list[HighRiskUnknown] = []
    for offset, line in enumerate(body_lines):
        line_number = declaration.line + offset
        stripped = line.strip()
        for match in DYN_TRAIT_RE.finditer(stripped):
            trait = match.group(1).split("::")[-1]
            unknowns.append(
                HighRiskUnknown(
                    "dynamic-dispatch",
                    declaration.relpath,
                    line_number,
                    stripped,
                    (
                        f'rg -n "impl .*{trait} for|dyn {trait}|Arc<dyn {trait}>|Box<dyn {trait}>" .',
                        f"{sys.argv[0]} --symbol {trait} --summary --write-report .sangoi/guard/rust-blast/{safe_filename(trait)}.json",
                    ),
                )
            )
        cfg_expressions = [match.group(1) for match in CFG_ATTR_RE.finditer(stripped)]
        cfg_expressions.extend(
            match.group(1) for match in CFG_MACRO_RE.finditer(stripped)
        )
        for cfg_expression in cfg_expressions:
            cfg_token = first_cfg_token(cfg_expression)
            unknowns.append(
                HighRiskUnknown(
                    "cfg-gated-target-body",
                    declaration.relpath,
                    line_number,
                    stripped,
                    (
                        f'rg -n "#\\[cfg\\([^]]*{cfg_token}|cfg!\\([^)]*{cfg_token}" {shlex.quote(declaration.relpath)}',
                        f'rg -n "{cfg_token}|target_os|target_family" .',
                    ),
                )
            )
        macro_match = MACRO_RULES_RE.match(stripped)
        if macro_match:
            macro_name = macro_match.group("name")
            unknowns.append(
                HighRiskUnknown(
                    "macro-generated-boundary",
                    declaration.relpath,
                    line_number,
                    stripped,
                    (f'rg -n "macro_rules!\\s+{macro_name}|{macro_name}!" .',),
                )
            )
    return unknowns


def first_cfg_token(cfg_expression: str) -> str:
    match = re.search(r"[A-Za-z_][A-Za-z0-9_]*", cfg_expression)
    return match.group(0) if match else "cfg"


def safe_filename(value: str) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]+", "__", value).strip("._") or "target"


def collect_atlas_overlays(config: Config, declaration: Declaration) -> list[Overlay]:
    agents_path = config.workspace / "AGENTS.md"
    if not agents_path.exists():
        return []
    matchers = overlay_matchers(declaration)
    overlays: list[Overlay] = []
    try:
        lines = agents_path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return overlays
    for line_number, line in enumerate(lines, start=1):
        reasons = tuple(reason for reason, pattern in matchers if pattern.search(line))
        if reasons:
            overlays.append(
                Overlay(
                    relpath="AGENTS.md",
                    line=line_number,
                    text=line.strip(),
                    reasons=reasons,
                )
            )
    return overlays


def overlay_matchers(declaration: Declaration) -> list[tuple[str, re.Pattern[str]]]:
    matchers: list[tuple[str, re.Pattern[str]]] = [
        (
            f"matched target path {declaration.relpath}",
            re.compile(re.escape(declaration.relpath)),
        ),
        (
            f"matched target file {Path(declaration.relpath).name}",
            re.compile(rf"\b{re.escape(Path(declaration.relpath).name)}\b"),
        ),
    ]
    if declaration.owner_type:
        matchers.append(
            (
                f"matched owner {declaration.owner_type}",
                re.compile(rf"\b{re.escape(declaration.owner_type)}\b"),
            )
        )
        matchers.append(
            (
                f"matched qualified symbol {declaration.owner_type}::{declaration.name}",
                re.compile(
                    rf"\b{re.escape(declaration.owner_type)}\s*::\s*{re.escape(declaration.name)}\b"
                ),
            )
        )
    elif len(declaration.name) > 5:
        matchers.append(
            (
                f"matched symbol {declaration.name}",
                re.compile(rf"\b{re.escape(declaration.name)}\b"),
            )
        )
    return matchers


def bucket_capping_warnings(
    buckets: dict[str, list[Hit]], overlays: Sequence[Overlay], max_hits: int | None
) -> list[str]:
    warnings: list[str] = []
    if max_hits is None:
        return warnings
    for bucket_name, hits in buckets.items():
        if len(hits) > max_hits:
            warnings.append(f"{bucket_name} capped at {max_hits} of {len(hits)} hits")
    if len(overlays) > max_hits:
        warnings.append(
            f"atlas candidate overlays capped at {max_hits} of {len(overlays)} hits"
        )
    return warnings


def target_candidate_capping_warnings(
    candidates: Sequence[Declaration], max_hits: int | None
) -> list[str]:
    if max_hits is None or len(candidates) <= max_hits:
        return []
    return [f"target candidates capped at {max_hits} of {len(candidates)} candidates"]


def suggested_next_commands(
    config: Config,
    declaration: Declaration,
    high_risk_unknowns: Sequence[HighRiskUnknown],
) -> list[str]:
    symbol = (
        f"{declaration.owner_type}::{declaration.name}"
        if declaration.owner_type
        else declaration.name
    )
    max_hits_suffix = (
        f" --max-hits {config.max_hits}" if config.max_hits is not None else ""
    )
    commands = [
        f"{sys.argv[0]} --workspace {shlex.quote(str(config.workspace))} --symbol {shlex.quote(symbol)} --json{max_hits_suffix}",
        f"{sys.argv[0]} --workspace {shlex.quote(str(config.workspace))} --file {shlex.quote(declaration.relpath)} --line {declaration.line} --summary --write-report .sangoi/guard/rust-blast/{safe_filename(symbol)}.json{max_hits_suffix}",
        f'rg -n "{re.escape(symbol)}|\\.{re.escape(declaration.name)}\\(" {shlex.quote(str(config.workspace))}',
    ]
    for unknown in high_risk_unknowns:
        commands.extend(
            command for command in unknown.follow_up_commands if command not in commands
        )
    return commands


def target_candidate_commands(
    config: Config, candidates: Sequence[Declaration]
) -> list[str]:
    return [file_line_command(config, candidate) for candidate in candidates]


def unresolved_next_commands(
    config: Config, candidates: Sequence[Declaration]
) -> list[str]:
    commands: list[str] = []
    if config.symbol:
        leaf = config.symbol.rsplit("::", 1)[-1]
        commands.append(
            f'rg -n "\\b{re.escape(leaf)}\\b" {shlex.quote(str(config.workspace))}'
        )
    commands.extend(target_candidate_commands(config, candidates))
    return commands


def file_line_command(config: Config, declaration: Declaration) -> str:
    max_hits_suffix = (
        f" --max-hits {config.max_hits}" if config.max_hits is not None else ""
    )
    return (
        f"{sys.argv[0]} --workspace {shlex.quote(str(config.workspace))} "
        f"--file {shlex.quote(declaration.relpath)} --line {declaration.line} --json{max_hits_suffix}"
    )


def target_to_json(config: Config, declaration: Declaration) -> dict[str, object]:
    return {
        "requested_symbol": config.symbol,
        "requested_file": relpath(config.workspace, config.target_file)
        if config.target_file
        else None,
        "requested_line": config.target_line,
        "resolved_symbol": declaration.qualified_name,
        "resolved_kind": declaration.kind,
        "resolution_note": declaration.resolution_note,
        "declaration": declaration_to_json(declaration, config),
    }


def declaration_to_json(
    declaration: Declaration, config: Config | None = None
) -> dict[str, object]:
    data: dict[str, object] = {
        "path": declaration.relpath,
        "line": declaration.line,
        "end_line": declaration.end_line,
        "name": declaration.name,
        "kind": declaration.kind,
        "owner_type": declaration.owner_type,
        "owner_trait": declaration.owner_trait,
        "qualified_name": declaration.qualified_name,
        "module_path": list(declaration.module_path),
        "signature": declaration.signature,
    }
    if declaration.attr_start_line and declaration.attr_start_line != declaration.line:
        data["attr_start_line"] = declaration.attr_start_line
    if declaration.attrs:
        data["attrs"] = list(declaration.attrs)
    if declaration.cfgs:
        data["cfgs"] = list(declaration.cfgs)
    if declaration.resolution_note:
        data["resolution_note"] = declaration.resolution_note
    if config:
        data["suggested_command"] = file_line_command(config, declaration)
    return data


def hit_bucket_to_json(hits: Sequence[Hit], max_hits: int | None) -> dict[str, object]:
    return {
        "total": len(hits),
        "shown": shown_count(hits, max_hits),
        "items": [hit_to_json(hit) for hit in limit_items(hits, max_hits)],
    }


def hit_to_json(hit: Hit) -> dict[str, object]:
    return {
        "path": hit.relpath,
        "line": hit.line,
        "text": hit.text,
        "matched_terms": sorted(hit.terms),
        "category": hit.category,
    }


def overlay_bucket_to_json(
    overlays: Sequence[Overlay], max_hits: int | None
) -> dict[str, object]:
    return {
        "total": len(overlays),
        "shown": shown_count(overlays, max_hits),
        "items": [
            dataclasses.asdict(overlay) for overlay in limit_items(overlays, max_hits)
        ],
    }


def shown_count(items: Sequence[object], max_hits: int | None) -> int:
    return len(items) if max_hits is None else min(len(items), max_hits)


def limit_items(items: Sequence[T], max_hits: int | None) -> Sequence[T]:
    return items if max_hits is None else items[:max_hits]


def is_capped(items: Sequence[object], max_hits: int | None) -> bool:
    return max_hits is not None and len(items) > max_hits


def write_report_if_requested(
    config: Config, report: dict[str, object]
) -> ReportWriteResult:
    if not config.write_report:
        return ReportWriteResult(None, None, None)
    config.write_report.parent.mkdir(parents=True, exist_ok=True)
    payload = json.dumps(report, indent=2, ensure_ascii=False, sort_keys=True) + "\n"
    config.write_report.write_text(payload, encoding="utf-8")
    digest = hashlib.sha256(payload.encode("utf-8")).hexdigest()
    return ReportWriteResult(
        str(config.write_report), digest, len(payload.encode("utf-8"))
    )


def summary_to_json(
    report: dict[str, object], write_result: ReportWriteResult
) -> dict[str, object]:
    inbound = report.get("inbound_references", {})
    inbound_totals = {}
    if isinstance(inbound, dict):
        inbound_totals = {
            name: bucket.get("total", 0)
            for name, bucket in inbound.items()
            if isinstance(bucket, dict)
        }
    target = report.get("target", {})
    return {
        "status": report.get("status"),
        "backend": report.get("backend"),
        "target": target,
        "report_path": write_result.path,
        "report_sha256": write_result.sha256,
        "report_bytes": write_result.bytes_written,
        "uncapped": report.get("report", {}).get("uncapped")
        if isinstance(report.get("report"), dict)
        else None,
        "inbound_totals": inbound_totals,
        "high_risk_unknowns": len(report.get("high_risk_unknowns", []))
        if isinstance(report.get("high_risk_unknowns"), list)
        else 0,
        "suggested_next_commands": report.get("suggested_next_commands", []),
        "warnings": report.get("warnings", []),
    }


def print_human_summary(
    report: dict[str, object], write_result: ReportWriteResult
) -> None:
    print("# Rust Blast Radius Guard Summary")
    print()
    target = report.get("target", {})
    resolved = None
    resolved_kind = None
    if isinstance(target, dict):
        resolved = (
            target.get("resolved_symbol")
            or target.get("requested_symbol")
            or target.get("requested_file")
        )
        resolved_kind = target.get("resolved_kind")
    print(f"- status: {report.get('status')}")
    print(f"- backend: {report.get('backend')}")
    if resolved:
        suffix = f" [{resolved_kind}]" if resolved_kind else ""
        print(f"- target: {resolved}{suffix}")
    report_meta = report.get("report", {})
    if isinstance(report_meta, dict):
        print(f"- uncapped: {str(report_meta.get('uncapped')).lower()}")
    if write_result.path:
        print(f"- report: {write_result.path}")
        print(f"- report_sha256: {write_result.sha256}")
        print(f"- report_bytes: {write_result.bytes_written}")

    inbound = report.get("inbound_references", {})
    print()
    print("## Inbound Totals")
    if isinstance(inbound, dict) and inbound:
        for bucket_name, bucket in inbound.items():
            if isinstance(bucket, dict):
                print(f"- {bucket_name}: {bucket.get('total', 0)}")
    else:
        print("- none")

    high_risk = report.get("high_risk_unknowns", [])
    print()
    print(
        f"## High-Risk Unknowns ({len(high_risk) if isinstance(high_risk, list) else 0})"
    )
    if isinstance(high_risk, list) and high_risk:
        for item in high_risk[:10]:
            print(
                f"- {item['kind']} at {item['relpath']}:{item['line']}: {item['evidence']}"
            )
        if len(high_risk) > 10:
            print(f"- ... {len(high_risk) - 10} more in full report")
    else:
        print("- none")

    commands = report.get("suggested_next_commands", [])
    print()
    print("## Suggested Next Commands")
    if isinstance(commands, list) and commands:
        for command in commands[:20]:
            print(f"- `{command}`")
        if len(commands) > 20:
            print(f"- ... {len(commands) - 20} more in full report")
    else:
        print("- none")


def print_human_report(report: dict[str, object]) -> None:
    print("# Rust Blast Radius Guard")
    print()
    print("## Target Resolution")
    target = report["target"]
    if isinstance(target, dict):
        print(f"- status: {report['status']}")
        print(f"- workspace: {report['workspace']}")
        print(f"- backend: {report['backend']}")
        if target.get("resolved_symbol"):
            kind = target.get("resolved_kind")
            suffix = f" [{kind}]" if kind else ""
            print(f"- resolved: {target['resolved_symbol']}{suffix}")
        if target.get("resolution_note"):
            print(f"- note: {target['resolution_note']}")
        if target.get("requested_symbol"):
            print(f"- requested symbol: {target['requested_symbol']}")
        if target.get("requested_file"):
            print(
                f"- requested file: {target['requested_file']}:{target.get('requested_line')}"
            )
        if "candidates" in target:
            candidates = target.get("candidates", [])
            shown = target.get(
                "candidate_shown",
                len(candidates) if isinstance(candidates, list) else 0,
            )
            total = target.get("candidate_total", shown)
            print(f"- candidates ({shown}/{total}):")
            if not candidates:
                print("  - none")
            for candidate in candidates:
                kind = candidate.get("kind")
                print(
                    f"  - {candidate['qualified_name']} [{kind}] at {candidate['path']}:{candidate['line']}"
                )
                if candidate.get("suggested_command"):
                    print(f"    command: `{candidate['suggested_command']}`")

    print()
    print("## Owner/Declaration")
    print_declarations(report.get("owner_declarations", []))

    print()
    print("## Inbound References")
    inbound = report.get("inbound_references", {})
    if isinstance(inbound, dict) and inbound:
        for bucket_name, bucket in inbound.items():
            print_hit_bucket(bucket_name, bucket)
    else:
        print("- none")

    print()
    print("## Outbound Tokens")
    outbound = report.get("outbound_tokens", {})
    if isinstance(outbound, dict) and outbound:
        for key in ("calls", "associated_paths", "types", "macros"):
            values = outbound.get(key, [])
            value_text = ", ".join(values) if values else "none"
            value_count = len(values) if isinstance(values, list) else 0
            print(f"- {key} ({value_count}): {value_text}")
    else:
        print("- none")

    print()
    print("## Atlas/Customization Candidate Overlays")
    print_overlay_bucket(report.get("atlas_candidate_overlays", {}))

    print()
    print("## High-Risk Unknowns")
    high_risk = report.get("high_risk_unknowns", [])
    if isinstance(high_risk, list) and high_risk:
        for item in high_risk:
            print(
                f"- {item['kind']} at {item['relpath']}:{item['line']}: {item['evidence']}"
            )
            for command in item.get("follow_up_commands", []):
                print(f"  follow-up: `{command}`")
    else:
        print("- none")

    print()
    print("## Suggested Next Commands")
    commands = report.get("suggested_next_commands", [])
    if isinstance(commands, list) and commands:
        for command in commands:
            print(f"- `{command}`")
    else:
        print("- none")


def print_declarations(raw_declarations: object) -> None:
    if not isinstance(raw_declarations, list) or not raw_declarations:
        print("- none")
        return
    for declaration in raw_declarations:
        kind = declaration.get("kind", "item")
        print(
            f"- {declaration['qualified_name']} [{kind}] at "
            f"{declaration['path']}:{declaration['line']}-{declaration['end_line']}"
        )
        print(f"  `{declaration['signature']}`")


def print_hit_bucket(bucket_name: str, raw_bucket: object) -> None:
    if not isinstance(raw_bucket, dict):
        return
    total = raw_bucket.get("total", 0)
    shown = raw_bucket.get("shown", 0)
    print(f"### {bucket_name} ({shown}/{total})")
    items = raw_bucket.get("items", [])
    if not isinstance(items, list) or not items:
        print("- none")
        return
    for item in items:
        terms = ", ".join(item.get("matched_terms", []))
        print(f"- {item['path']}:{item['line']} [{terms}]")
        print(f"  `{item['text']}`")


def print_overlay_bucket(raw_bucket: object) -> None:
    if not isinstance(raw_bucket, dict):
        print("- none")
        return
    total = raw_bucket.get("total", 0)
    shown = raw_bucket.get("shown", 0)
    print(f"- showing {shown}/{total}")
    items = raw_bucket.get("items", [])
    if not isinstance(items, list) or not items:
        print("- none")
        return
    for item in items:
        reasons = "; ".join(item.get("reasons", []))
        print(f"- {item['relpath']}:{item['line']} [{reasons}]")
        print(f"  `{item['text']}`")


def relpath(workspace: Path, path: Path | None) -> str:
    if path is None:
        return ""
    try:
        return path.resolve().relative_to(workspace).as_posix()
    except ValueError:
        return path.as_posix()


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
