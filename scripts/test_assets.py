#!/usr/bin/env python3
"""Run the `animated2d` CLI against every character in a local assets folder.

The folder is `--assets`, else the `A2D_TEST_ASSETS` environment variable, else
`<repo>/Test Assets` (gitignored: real assets stay local). Only the direct
children are scanned, never recursed; hidden entries, `.gitkeep` and README files
are skipped. One entry is one character:

  * a directory (a `.skel.bytes` / `.atlas.txt` / `.png` set, or Spine JSON plus
    atlas), or
  * a file with no recognised loose-set extension (a Unity AssetBundle), or
  * a group of loose files sharing a stem (`x.skel.bytes`, `x.atlas.txt`, `x.png`
    form one entry `x`, run on the skeleton member, else the first by name; extra
    atlas pages such as `x2.png` are not attached, so put a multi-page character
    in a directory).

Per character, in order:

    1. inspect   <entry>
    2. import    <entry> -o <run>/packages/<key>.a2dpack
    3. validate  <package>
    4. preview   <package> -o <run>/frames/<key>      (--render, needs a GPU)
    5. preview   <package> --exit-after <seconds>     (--view, opens the viewer)

`<run>` is a fresh `<out>/<YYYYmmdd-HHMMSS>/` per run (nothing existing is ever
deleted; `--out` must not be, contain or sit inside the assets folder). `<key>` is
`<index>-<sanitised name>`, unique per entry, and is recorded in
`summary.json`. Step output goes to `<run>/logs/<key>/<step>.log`; a table is
printed at the end and `<run>/summary.json` holds the details of every entry.

Pointing it at a big dump (thousands of entries, most not characters): select by
name with `--include` / `--exclude`, sample with `--limit`, and add
`--skip-unrecognized` so entries that are evidently not an asset the CLI reads are
`skipped` instead of `failed`. That is a heuristic on CLI messages: a loose
skeleton file whose detection is ambiguous or corrupt currently also surfaces as
"is not a Spine skeleton" and is skipped (a CLI gap, so check logs of skipped
files when it matters). Packages are deleted once done unless `--keep-packages`,
or `--view` (deleted after viewing); logs are always kept.

Exit code: 0 when every character is `ok` or `skipped`; 1 when some have
validation problems but none failed; 2 when anything failed (inspect/import/
validate error or timeout, a requested render or view failed, a harness error),
or the run itself could not proceed (build failure, bad `--game`, bad folder).
`--allow-problems` makes problems exit 0.

Examples:

    python scripts/test_assets.py
    python scripts/test_assets.py --only hero --render
    python scripts/test_assets.py --assets D:/dump --jobs 4 --timeout 120
    A2D_TEST_ASSETS=D:/dump python scripts/test_assets.py --include "live2d_*" --include "spine_*" --limit 20 --skip-unrecognized
    python scripts/test_assets.py --view 5 --game unity_cubism
    python scripts/test_assets.py --toolchain stable
    python scripts/test_assets.py --bin target/release/animated2d
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
import re
import shutil
import subprocess
import sys
import threading
import time
import traceback
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parent.parent
SKIPPED_NAMES = {".gitkeep", "readme", "readme.md", "readme.txt"}
FULL_TABLE_LIMIT = 50
# Mirrors the CLI's normalize_asset_stem (crates/a2d-import/src/generic.rs): at most two
# of these suffixes are stripped to find a loose file's character name.
STRIPPABLE = {"bytes", "txt", "skel", "atlas", "json", "png", "webp", "jpg", "jpeg"}
SKELETON_SUFFIXES = {"skel", "json"}

# Markers of "this is not a character", matched against CLI output. If the CLI's
# wording changes, update these:
#   UNDETERMINED      crates/a2d-cli/src/commands.rs, inspect_unity_bundle (`Importer: (undetermined)`)
#   FOUND_NEITHER     crates/a2d-import/src/games.rs, the bundle reconstruction error
#   NOT_A_SKELETON    crates/a2d-import/src/detect.rs, require_spine_skeleton (and the generic importer's
#                     missing-skeleton error that wraps it)
#   NO_SKELETON_IN    crates/a2d-import/src/generic.rs, a directory with no skeleton
# Run-level error (a bad --game name), crates/a2d-cli/src/commands.rs named_importer:
#   UNKNOWN_IMPORTER
UNDETERMINED = "Importer: (undetermined)"
FOUND_NEITHER = "found neither"
NOT_A_SKELETON = "is not a Spine skeleton"
NO_SKELETON_IN = "no Spine skeleton found in"
UNKNOWN_IMPORTER = "unknown importer"


class SetupError(Exception):
    """The run cannot proceed (build failure, bad --game ...): exit 2."""


def find_binary(toolchain: str | None) -> Path:
    """Builds `a2d-cli` in release mode and returns the produced executable."""
    cargo = ["cargo"] + ([f"+{toolchain}"] if toolchain else [])
    print("Building animated2d (release)...", flush=True)
    build = subprocess.run(cargo + ["build", "--release", "-p", "a2d-cli"], cwd=REPO_ROOT)
    if build.returncode != 0:
        raise SetupError(f"cargo build failed with exit code {build.returncode}")
    meta = subprocess.run(
        cargo + ["metadata", "--format-version", "1", "--no-deps"],
        cwd=REPO_ROOT, capture_output=True, text=True, encoding="utf-8",
    )
    if meta.returncode != 0:
        raise SetupError(f"cargo metadata failed: {meta.stderr.strip()}")
    target = Path(json.loads(meta.stdout)["target_directory"])
    binary = target / "release" / ("animated2d.exe" if os.name == "nt" else "animated2d")
    if not binary.is_file():
        raise SetupError(f"expected binary not found at {binary}")
    return binary


def loose_parts(name: str) -> tuple[str, bool] | None:
    """(character stem, is a skeleton member) for a loose-set file, else None.

    Strips at most two suffixes like the CLI does; a file is loose when at least one
    was stripped. `x.skel.bytes` -> ("x", True), `x.png` -> ("x", False).
    """
    stem, stripped = name, []
    for _ in range(2):
        head, dot, ext = stem.rpartition(".")
        if not dot or not head or ext.lower() not in STRIPPABLE:
            break
        stem = head
        stripped.append(ext.lower())
    return (stem, bool(SKELETON_SUFFIXES & set(stripped))) if stripped else None


def discover(assets: Path, only: list[str], include: list[str], exclude: list[str],
             limit: int | None) -> list[tuple[str, Path]]:
    """(display name, path to run on) per character: direct children only, grouped and sorted.

    Loose files sharing a stem form one entry, run on the skeleton member (else the
    first by name). Extra atlas pages (`hero2.png`) are not attached to their
    character: put a multi-page character in its own directory.
    """
    with os.scandir(assets) as it:
        children = [Path(e.path) for e in it if not e.name.startswith(".") and e.name.lower() not in SKIPPED_NAMES]
    children.sort(key=lambda p: p.name.lower())
    plain: dict[str, Path] = {}
    loose: dict[str, tuple[Path, bool]] = {}  # stem -> (path to run on, is a skeleton)
    for child in children:
        parts = loose_parts(child.name) if child.is_file() else None
        if parts is None:
            plain[child.name] = child
            continue
        stem, is_skeleton = parts
        current = loose.get(stem)
        if current is None or (is_skeleton and not current[1]):
            loose[stem] = (child, is_skeleton)
    entries = dict(plain)
    for stem, (path, _) in loose.items():
        name = stem
        while name in entries:  # a directory or bundle already uses the stem
            name += " (loose)"
        entries[name] = path
    inc = [g.lower() for g in include]
    exc = [g.lower() for g in exclude]
    selected = []
    for name, path in sorted(entries.items(), key=lambda kv: kv[0].lower()):
        low = name.lower()
        if only and not any(s.lower() in low for s in only):
            continue
        if inc and not any(fnmatch.fnmatchcase(low, g) for g in inc):
            continue
        if any(fnmatch.fnmatchcase(low, g) for g in exc):
            continue
        selected.append((name, path))
    return selected[:limit] if limit is not None else selected


def safe_name(name: str) -> str:
    return re.sub(r"[^A-Za-z0-9._-]+", "_", name).strip("._") or "character"


def run_step(binary: Path, args: list[str], log: Path, timeout: float) -> dict[str, Any]:
    """Runs one CLI invocation, writes its output to `log`, returns the outcome."""
    started = time.monotonic()
    command = [str(binary)] + args
    timed_out = False
    try:
        proc = subprocess.run(command, capture_output=True, timeout=timeout)
        code: int | None = proc.returncode
        stdout, stderr = proc.stdout, proc.stderr
    except subprocess.TimeoutExpired as e:
        timed_out, code = True, None
        stdout, stderr = e.stdout or b"", e.stderr or b""
    except OSError as e:
        code, stdout, stderr = None, b"", str(e).encode()
    out = stdout.decode("utf-8", "replace")
    err = stderr.decode("utf-8", "replace")
    log.parent.mkdir(parents=True, exist_ok=True)
    log.write_text(
        f"$ {' '.join(command)}\n"
        f"exit: {'timeout' if timed_out else code}\n\n--- stdout ---\n{out}\n--- stderr ---\n{err}",
        encoding="utf-8",
    )
    return {"exit_code": code, "timed_out": timed_out, "seconds": round(time.monotonic() - started, 2),
            "stdout": out, "stderr": err}


def parse_validate(stdout: str) -> tuple[list[str], list[str]]:
    """Returns (problem lines, information lines) from `validate` output."""
    problems: list[str] = []
    info: list[str] = []
    section: list[str] | None = None
    for line in stdout.splitlines():
        if line.startswith("Problems:"):
            section = problems
        elif line.startswith("Recorded at import"):
            section = info
        elif line.startswith("- "):
            if section is not None:
                section.append(line[2:])
        elif line.strip():
            section = None
    return problems, info


def summarize(outcome: dict[str, Any]) -> dict[str, Any]:
    return {k: v for k, v in outcome.items() if k not in ("stdout", "stderr")}


def run_level_error(outcome: dict[str, Any]) -> str | None:
    """A message when a step failed because of the invocation (a bad --game), not the asset."""
    err = outcome["stderr"]
    if outcome["exit_code"] == 3 and UNKNOWN_IMPORTER in err:
        return err.strip().splitlines()[-1] if err.strip() else "bad --game"
    return None


def is_unrecognized(label: str, outcome: dict[str, Any]) -> bool:
    """True when a failed step means the input is not an asset the CLI reads.

    The CLI exits 3 on every load error, so the message decides. A recognised
    asset (inspect already printed a `Character:` line or classified a file as a
    Spine skeleton or atlas) is never skipped. "unsupported format:" is deliberately not a
    marker: it also reports real decode failures of recognised skeletons.
    """
    if outcome["timed_out"] or outcome["exit_code"] != 3:
        return False
    err, out = outcome["stderr"], outcome["stdout"]
    if "Character:" in out or "spine skeleton" in out or "spine atlas" in out:
        return False
    if label == "inspect":
        return NOT_A_SKELETON in err or NO_SKELETON_IN in err
    return label == "import" and FOUND_NEITHER in err


def test_character(binary: Path, name: str, path: Path, key: str, out: Path, args: argparse.Namespace,
                   abort: threading.Event) -> dict[str, Any]:
    logs = out / "logs" / key
    package = out / "packages" / f"{key}.a2dpack"
    game = ["--game", args.game] if args.game else []
    result: dict[str, Any] = {
        "character": name, "key": key, "path": str(path), "status": "failed", "failing_step": None,
        "render": "skipped", "view": "skipped", "problems": [], "information": [], "warnings": [], "steps": {},
    }

    def step(label: str, cli: list[str], timeout: float | None = None) -> dict[str, Any]:
        outcome = run_step(binary, cli, logs / f"{label}.log", timeout or args.timeout)
        result["steps"][label] = summarize(outcome)
        return outcome

    def failed(outcome: dict[str, Any]) -> bool:
        return outcome["timed_out"] or outcome["exit_code"] != 0

    def drop_package() -> None:
        shutil.rmtree(package, ignore_errors=True)

    if abort.is_set():
        result["failing_step"] = "aborted"
        return result
    package.parent.mkdir(parents=True, exist_ok=True)

    for label, cli in (
        ("inspect", ["inspect", str(path)] + game),
        ("import", ["import", str(path), "-o", str(package)] + game),
    ):
        outcome = step(label, cli)
        if failed(outcome):
            message = run_level_error(outcome)
            if message:
                result["run_error"] = message
                abort.set()
            elif args.skip_unrecognized and is_unrecognized(label, outcome):
                result["status"] = "skipped"
                return result
            result["failing_step"] = label
            drop_package()
            return result
        # A bundle with neither a Cubism model nor a Spine rig inspects cleanly but
        # reports no importer; skip it before the (costlier) import.
        if args.skip_unrecognized and label == "inspect" and UNDETERMINED in outcome["stdout"]:
            result["status"] = "skipped"
            return result

    validated = step("validate", ["validate", str(package)])
    if validated["timed_out"] or validated["exit_code"] not in (0, 1):
        result["failing_step"] = "validate"
        drop_package()
        return result
    result["problems"], result["information"] = parse_validate(validated["stdout"])
    result["status"] = "ok" if validated["exit_code"] == 0 else "problems"
    if result["status"] == "problems" and not result["problems"]:
        result["warnings"].append("validate exited 1 but no `Problems:` lines were parsed (CLI wording changed?)")

    if args.render:
        frames = out / "frames" / key
        shutil.rmtree(frames, ignore_errors=True)
        rendered = step("render", ["preview", str(package), "-o", str(frames)])
        result["render"] = "failed" if failed(rendered) else "ok"
    if args.view:
        # The viewer runs for `--view` seconds; leave room for a cold GPU start.
        viewed = step("view", ["preview", str(package), "--exit-after", str(args.view)], args.view + args.timeout)
        result["view"] = "failed" if failed(viewed) else "ok"
    if not args.keep_packages:
        drop_package()
    return result


def print_table(results: list[dict[str, Any]], args: argparse.Namespace, full: bool) -> None:
    counts = {k: sum(r["status"] == k for r in results) for k in ("ok", "problems", "failed", "skipped")}
    header = ["character", "status", "problems", "info", "failing step"]
    header += (["render"] if args.render else []) + (["view"] if args.view else [])
    rows = [header]
    for r in results:
        bad = r["status"] in ("problems", "failed") or "failed" in (r["render"], r["view"])
        if bad or (full and r["status"] != "skipped"):
            row = [r["character"], r["status"], str(len(r["problems"])), str(len(r["information"])),
                   r["failing_step"] or "-"]
            row += ([r["render"]] if args.render else []) + ([r["view"]] if args.view else [])
            rows.append(row)
    print()
    if len(rows) > 1:
        widths = [max(len(row[i]) for row in rows) for i in range(len(header))]
        for i, row in enumerate(rows):
            print("  ".join(cell.ljust(widths[c]) for c, cell in enumerate(row)).rstrip())
            if i == 0:
                print("  ".join("-" * w for w in widths))
        print()
    print(f"total {len(results)}: " + ", ".join(f"{v} {k}" for k, v in counts.items()))


def positive(kind: type) -> Any:
    def parse(text: str) -> Any:
        try:
            value = kind(text)
        except ValueError:
            raise argparse.ArgumentTypeError(f"not a number: {text!r}")
        if value <= 0:
            raise argparse.ArgumentTypeError("must be greater than 0")
        return value
    return parse


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run animated2d against every character in a Test Assets folder.",
        epilog="Exit code: 0 all ok (or skipped); 1 some have validation problems; 2 some failed or the run could not proceed.",
    )
    parser.add_argument("--assets", type=Path,
                        default=Path(os.environ.get("A2D_TEST_ASSETS") or REPO_ROOT / "Test Assets"),
                        help="folder whose direct children are characters "
                             "(default: $A2D_TEST_ASSETS, else <repo>/Test Assets)")
    parser.add_argument("--out", type=Path, default=REPO_ROOT / "target" / "asset-test",
                        help="parent folder for run output; each run gets a fresh <out>/<YYYYmmdd-HHMMSS>/ holding "
                             "logs/, packages/, frames/ and summary.json, and nothing existing is deleted. "
                             "Must not be, contain or sit inside the assets folder (default: <repo>/target/asset-test)")
    parser.add_argument("--only", action="append", default=[], metavar="SUBSTR",
                        help="only characters whose name contains SUBSTR (repeatable, case-insensitive)")
    parser.add_argument("--include", action="append", default=[], metavar="GLOB",
                        help='only entries whose name matches GLOB (repeatable, case-insensitive), e.g. "live2d_*"')
    parser.add_argument("--exclude", action="append", default=[], metavar="GLOB",
                        help="skip entries whose name matches GLOB (repeatable, case-insensitive)")
    parser.add_argument("--limit", type=positive(int), metavar="N",
                        help="stop after N selected entries (after sorting and filtering)")
    parser.add_argument("--skip-unrecognized", action="store_true",
                        help="entries that are evidently not an asset the CLI reads are skipped, not failed")
    parser.add_argument("--keep-packages", action="store_true",
                        help="keep each .a2dpack (default: deleted when the character is done)")
    parser.add_argument("--full-table", action="store_true",
                        help=f"list every character in the final table (default above {FULL_TABLE_LIMIT}: only non-ok)")
    parser.add_argument("--bin", type=Path, help="use this animated2d binary instead of building one")
    parser.add_argument("--toolchain", help="build with `cargo +<toolchain>`")
    parser.add_argument("--game", help="pass --game <name> to inspect and import (a bad name aborts the run)")
    parser.add_argument("--render", action="store_true", help="also render frames with `preview -o` (needs a GPU)")
    parser.add_argument("--view", type=positive(float), metavar="SECONDS",
                        help="open each character in the real viewer for SECONDS, one after another (forces --jobs 1)")
    parser.add_argument("--timeout", type=positive(float), default=300.0, help="per-step timeout in seconds (default: 300)")
    parser.add_argument("--jobs", type=positive(int), default=1, help="characters processed in parallel (default: 1)")
    parser.add_argument("--allow-problems", action="store_true", help="exit 0 when characters only have problems")
    return parser.parse_args()


def run(args: argparse.Namespace) -> int:
    assets = args.assets.resolve()
    if not assets.is_dir():
        print(f"error: assets folder not found: {assets}\n"
              "Create it and put one character per file or folder inside, or set --assets / A2D_TEST_ASSETS.",
              file=sys.stderr)
        return 2
    entries = discover(assets, args.only, args.include, args.exclude, args.limit)
    if not entries:
        print(f"error: no characters selected in {assets}", file=sys.stderr)
        return 2

    out_base = args.out.resolve()
    if out_base == assets or assets in out_base.parents or out_base in assets.parents:
        raise SetupError(f"--out ({out_base}) must not be, contain or sit inside the assets folder ({assets})")
    binary = args.bin.resolve() if args.bin else find_binary(args.toolchain)
    if os.name == "nt" and not binary.is_file() and not binary.suffix:
        binary = binary.with_suffix(".exe")
    if not binary.is_file():
        raise SetupError(f"binary not found: {binary}")
    # Every run writes into a fresh directory; nothing that already exists is touched.
    stamp = time.strftime("%Y%m%d-%H%M%S")
    out, n = out_base / stamp, 1
    while out.exists():
        n += 1
        out = out_base / f"{stamp}-{n}"
    out.mkdir(parents=True)
    jobs = 1 if args.view else args.jobs
    print(f"{len(entries)} entr{'y' if len(entries) == 1 else 'ies'} selected, jobs={jobs}", flush=True)

    done = 0
    lock = threading.Lock()
    abort = threading.Event()

    def work(item: tuple[int, tuple[str, Path]]) -> dict[str, Any]:
        nonlocal done
        index, (name, path) = item
        key = f"{index:05d}-{safe_name(name)}"
        try:
            result = test_character(binary, name, path, key, out, args, abort)
        except Exception:  # keep the run going; the harness bug is recorded on the entry
            result = {"character": name, "key": key, "path": str(path), "status": "failed",
                      "failing_step": "harness", "render": "skipped", "view": "skipped", "problems": [],
                      "information": [], "warnings": [], "steps": {}, "harness_error": traceback.format_exc()}
        with lock:
            done += 1
            note = f" (failed at {result['failing_step']})" if result["failing_step"] else ""
            note += "".join(f" [warning: {w}]" for w in result["warnings"])
            print(f"[{done}/{len(entries)}] {name} {result['status']}{note}", flush=True)
        return result

    with ThreadPoolExecutor(max_workers=jobs) as pool:
        results = list(pool.map(work, enumerate(entries)))  # map keeps the sorted order

    print_table(results, args, args.full_table or len(results) <= FULL_TABLE_LIMIT)
    (out / "summary.json").write_text(
        json.dumps({"assets": str(assets), "binary": str(binary), "results": results}, indent=2, ensure_ascii=False),
        encoding="utf-8",
    )
    print(f"Run directory: {out}")

    run_errors = [r["run_error"] for r in results if "run_error" in r]
    if run_errors:
        print(f"error: the run cannot proceed: {run_errors[0]}", file=sys.stderr)
        return 2
    if any(r["status"] == "failed" or "failed" in (r["render"], r["view"]) for r in results):
        return 2
    if any(r["status"] == "problems" for r in results) and not args.allow_problems:
        return 1
    return 0


def main() -> int:
    # Redirected output on Windows uses the legacy code page; never die on a name.
    for stream in (sys.stdout, sys.stderr):
        stream.reconfigure(errors="backslashreplace")
    args = parse_args()
    try:
        return run(args)
    except SetupError as e:
        print(f"error: {e}", file=sys.stderr)
        return 2
    except Exception:
        traceback.print_exc()
        return 2


if __name__ == "__main__":
    sys.exit(main())
