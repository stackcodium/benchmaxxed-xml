#!/usr/bin/env python3
from __future__ import annotations

import hashlib
import csv
import json
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REQUIRED = [
    ".nojekyll", "README.md", "RUNNING.md", "LICENSE",
    "parser/Cargo.toml", "benchmarks/bun-xml-bench.ts", "benchmarks/php-xml-bench.php",
    "benchmarks/python-xml-bench.py", "benchmarks/pugixml_bench.cpp",
    "benchmarks/quickxml-bench/Cargo.toml", "datasets/canada.xml",
    "scripts/capture-environment.py",
    "datasets/citm_catalog.xml", "datasets/twitter.xml", "reports/latest/xml-parser-report.html",
    "reports/latest/results.tsv", "reports/latest/process-metrics.tsv",
    "reports/latest/environment.json", "provenance/references.toml", "provenance/datasets.toml",
    "provenance/VERIFICATION.md",
]
FORBIDDEN_TEXT = [re.compile(pattern) for pattern in [
    "/" + "home/", "zen-" + r"garten\.net", "serviono-" + "references",
    "YOUR_GITHUB_" + "ACCOUNT",
    "-----BEGIN " + r"(?:RSA |EC |OPENSSH )?PRIVATE KEY-----",
    r"(?:ghp|github_pat)_[A-Za-z0-9_]{20,}",
    r"AKIA[0-9A-Z]{16}",
    r"https?://[^/\s:@]+:[^/\s@]+@",
]]
TEXT_SUFFIXES = {".c", ".cc", ".cpp", ".h", ".html", ".json", ".md", ".py", ".rs", ".sh", ".toml", ".ts", ".txt"}

errors: list[str] = []
for relative in REQUIRED:
    if not (ROOT / relative).is_file(): errors.append(f"missing required file: {relative}")

for path in ROOT.rglob("*"):
    relative = path.relative_to(ROOT)
    if relative.parts and relative.parts[0] == ".git":
        continue
    if relative.parts and relative.parts[0] in {".local", "references"}:
        continue
    if "target" in relative.parts or "__pycache__" in relative.parts:
        continue
    if path.is_symlink(): errors.append(f"symlink is not allowed: {relative}")
    if path.is_dir() and path.name == ".git":
        errors.append(f"generated or repository directory is not allowed: {relative}")
    if path.is_file() and path.stat().st_size >= 100 * 1024 * 1024:
        errors.append(f"file exceeds GitHub's 100 MiB limit: {relative}")
    if path.is_file() and (path.suffix in TEXT_SUFFIXES or path.name == "Cargo.lock"):
        try: text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError: continue
        for pattern in FORBIDDEN_TEXT:
            if pattern.search(text): errors.append(f"forbidden local/private text in {relative}: {pattern.pattern}")

expected_references = {
    "quick_xml": ("https://github.com/tafia/quick-xml", "v0.41.0", "4deda08abeffdc188c269360229cf47e12a77a9f"),
    "pugixml": ("https://github.com/zeux/pugixml", "v1.16-1-g27b6832", "27b68329de32cf9c601ca8eb6c588fd639960c40"),
    "bun": ("https://github.com/oven-sh/bun", "1.4.0-canary.1+1dd66afde", "1dd66afde213732c645c60ac08cf68f1087a271d"),
}
references_path = ROOT / "provenance/references.toml"
if references_path.is_file():
    with references_path.open("rb") as handle: references = tomllib.load(handle)
    if references.get("package_version") != "0.1.0":
        errors.append("package version does not match the published release")
    expected_runtimes = {"php": "8.5.9", "python": "3.13.14"}
    for key, version in expected_runtimes.items():
        if references.get("runtimes", {}).get(key, {}).get("version") != version:
            errors.append(f"runtime version lock mismatch: {key}")
    for key, (url, version, revision) in expected_references.items():
        actual = references.get("references", {}).get(key, {})
        if (actual.get("url"), actual.get("version"), actual.get("revision")) != (url, version, revision):
            errors.append(f"reference lock mismatch: {key}")

results_path = ROOT / "reports/latest/results.tsv"
if results_path.is_file():
    with results_path.open(newline="", encoding="utf-8") as handle:
        rows = list(csv.DictReader(handle, delimiter="\t"))
    by_dataset: dict[str, set[str]] = {}
    for row in rows: by_dataset.setdefault(row["dataset"], set()).add(row["checksum"])
    for dataset, checksums in by_dataset.items():
        if len(checksums) != 1: errors.append(f"published structural checksum disagreement: {dataset}")

expected_datasets = {
    "canada": "9935f28789fdff34d9294a3248e49b82bc307add47009d222735f9b06c3f593e",
    "citm_catalog": "514cc6c1805e4d66910c4188ef9139d4e577dec13d90b689a47727b4083d72ec",
    "twitter": "3209ef389a085949432e40458cb84a46d4fbe658d8fa2afcf7a314efb0ed716e",
}
datasets_path = ROOT / "provenance/datasets.toml"
if datasets_path.is_file():
    with datasets_path.open("rb") as handle: dataset_data = tomllib.load(handle)
    recorded = {item["name"]: item["sha256"] for item in dataset_data.get("dataset", [])}
    if recorded != expected_datasets: errors.append("dataset provenance hashes do not match the published corpus")
    for name, expected in expected_datasets.items():
        path = ROOT / "datasets" / f"{name}.xml"
        if path.is_file() and hashlib.sha256(path.read_bytes()).hexdigest() != expected:
            errors.append(f"published dataset hash mismatch: {name}")

environment_path = ROOT / "reports/latest/environment.json"
if environment_path.is_file():
    environment = json.loads(environment_path.read_text(encoding="utf-8"))
    parser_source = environment.get("source", {}).get("parser") or {}
    if parser_source.get("revision") != "7e3d7f924bd94d6aefb49d63f8fa81eb09e4021b" or parser_source.get("dirty"):
        errors.append("published report parser provenance is not the clean pinned revision")
    environment_references = environment.get("source", {}).get("references", {})
    for key, (_, _, revision) in expected_references.items():
        if key == "bun":
            continue
        if environment_references.get(key, {}).get("revision") != revision:
            errors.append(f"published report environment reference mismatch: {key}")
    if environment.get("tools", {}).get("bun") != "1.4.0-canary.1+1dd66afde":
        errors.append("published report Bun runtime mismatch")
    if environment.get("tools", {}).get("php") != "8.5.9":
        errors.append("published report PHP runtime mismatch")
    if environment.get("tools", {}).get("python") != "3.13.14":
        errors.append("published report Python runtime mismatch")
    expected_parsers = {
        "benchmaxxed-xml-walk", "pugixml-minimal-walk", "quickxml-event-walk",
        "bun-xml-ordered-walk", "php-libxml2-compact-walk", "python-libxml2-compact-walk",
    }
    if set(environment.get("benchmark", {}).get("parsers", [])) != expected_parsers:
        errors.append("published report does not contain the full parser set")
    for key, relative in (("results", "reports/latest/results.tsv"), ("metrics", "reports/latest/process-metrics.tsv")):
        expected = environment.get("artifacts", {}).get(key, {}).get("sha256")
        path = ROOT / relative
        if path.is_file() and expected != hashlib.sha256(path.read_bytes()).hexdigest():
            errors.append(f"published report artifact provenance mismatch: {key}")
if errors:
    print("package verification failed:", file=sys.stderr)
    for error in errors: print(f"- {error}", file=sys.stderr)
    raise SystemExit(1)
print(f"package verification passed: {ROOT}")
