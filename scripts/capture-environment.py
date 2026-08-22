#!/usr/bin/env python3
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import platform
import subprocess
import tomllib
from pathlib import Path


def command(*args: str) -> str:
    try:
        completed = subprocess.run(args, check=True, text=True, capture_output=True)
        return completed.stdout.strip().splitlines()[0]
    except (OSError, subprocess.CalledProcessError, IndexError):
        return "unavailable"


def git_state(path: Path | None) -> dict[str, object] | None:
    if path is None or not (path / ".git").exists():
        return None
    status = subprocess.run(
        ("git", "-C", str(path), "status", "--porcelain"),
        check=True,
        text=True,
        capture_output=True,
    )
    return {
        "revision": command("git", "-C", str(path), "rev-parse", "HEAD"),
        "dirty": bool(status.stdout.strip()),
    }


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def system_name() -> str:
    try:
        for line in Path("/etc/os-release").read_text(encoding="utf-8").splitlines():
            if line.startswith("NAME="):
                return line.split("=", 1)[1].strip().strip('"')
    except OSError:
        pass
    return platform.system()


def parser_snapshot_state(parser_dir: Path, suite_dir: Path | None) -> dict[str, object] | None:
    if suite_dir is None:
        return None
    provenance_path = suite_dir / "provenance/references.toml"
    if not provenance_path.is_file():
        return None
    with provenance_path.open("rb") as handle:
        revision = tomllib.load(handle).get("parser", {}).get("revision")
    if not revision:
        return None
    try:
        relative_parser = parser_dir.resolve().relative_to(suite_dir.resolve()).as_posix()
    except ValueError:
        return None
    dirty: bool | None = None
    verification = "unavailable"
    if (suite_dir / ".git").exists():
        status = subprocess.run(
            ("git", "-C", str(suite_dir), "status", "--porcelain", "--", relative_parser),
            check=True,
            text=True,
            capture_output=True,
        )
        dirty = bool(status.stdout.strip())
        verification = "git"
    return {
        "revision": revision,
        "dirty": dirty,
        "snapshot": True,
        "verification": verification,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description="Capture the environment for one XML benchmark report.")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--results", type=Path, required=True)
    parser.add_argument("--metrics", type=Path, required=True)
    parser.add_argument("--parser-dir", type=Path, required=True)
    parser.add_argument("--suite-dir", type=Path)
    parser.add_argument("--quickxml-dir", type=Path)
    parser.add_argument("--pugixml-dir", type=Path)
    parser.add_argument("--dataset-source-dir", type=Path)
    parser.add_argument("--bun-bin", default="bun")
    parser.add_argument("--php-bin", default="php")
    parser.add_argument("--python-bin", default="python3")
    parser.add_argument("--parsers", required=True)
    parser.add_argument("--runs", type=int, required=True)
    parser.add_argument("--minimum-duration-ms", type=int, required=True)
    parser.add_argument("--warmup", type=int, required=True)
    parser.add_argument("--iterations", type=int, required=True)
    parser.add_argument("--cooldown-seconds", type=float, required=True)
    args = parser.parse_args()

    cpu = platform.processor()
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.is_file():
        for line in cpuinfo.read_text(errors="replace").splitlines():
            if line.lower().startswith(("model name", "hardware")):
                cpu = line.split(":", 1)[-1].strip()
                break
    if not cpu or cpu == platform.machine():
        try:
            output = subprocess.run(
                ("lscpu",), check=True, text=True, capture_output=True
            ).stdout
            for line in output.splitlines():
                if line.lower().startswith("model name:"):
                    cpu = line.split(":", 1)[1].strip()
                    break
        except (OSError, subprocess.CalledProcessError):
            pass

    references = {
        name: state
        for name, state in {
            "quick_xml": git_state(args.quickxml_dir),
            "pugixml": git_state(args.pugixml_dir),
            "dataset_source": git_state(args.dataset_source_dir),
        }.items()
        if state is not None
    }
    parser_state = git_state(args.parser_dir)
    if parser_state is None:
        parser_state = parser_snapshot_state(args.parser_dir, args.suite_dir)

    data = {
        "captured_utc": dt.datetime.now(dt.UTC).isoformat(),
        "host": {
            "system": system_name(),
            "architecture": platform.machine(),
            "cpu": cpu,
        },
        "source": {
            "parser": parser_state,
            "suite": git_state(args.suite_dir),
            "references": references,
        },
        "tools": {
            "bun": command(args.bun_bin, "--revision"),
            "rustc": command("rustc", "--version"),
            "cargo": command("cargo", "--version"),
            "cxx": command("c++", "--version"),
            "php": command(args.php_bin, "-r", "echo PHP_VERSION;"),
            "python": command(args.python_bin, "--version"),
            "gnu_time": command("/usr/bin/time", "--version"),
        },
        "benchmark": {
            "parsers": [value for value in args.parsers.split(",") if value],
            "runs": args.runs,
            "minimum_duration_ms": args.minimum_duration_ms,
            "warmup": args.warmup,
            "initial_iterations": args.iterations,
            "cooldown_seconds": args.cooldown_seconds,
        },
        "artifacts": {
            "results": {"path": args.results.name, "sha256": sha256(args.results)},
            "metrics": {"path": args.metrics.name, "sha256": sha256(args.metrics)},
        },
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"captured {args.out}")


if __name__ == "__main__":
    main()
