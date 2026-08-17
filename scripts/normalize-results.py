#!/usr/bin/env python3
import csv
import sys
from pathlib import Path

out_dir = Path(sys.argv[1])
parsers = sys.argv[2:] or [
    "benchmaxxed-xml-walk",
    "pugixml-minimal-walk",
    "quickxml-event-walk",
    "bun-xml-ordered-walk",
    "php-libxml2-compact-walk",
    "python-libxml2-compact-walk",
]
fields = ["parser", "dataset", "input_bytes", "iterations", "best_ms", "mb_s", "checksum"]

with (out_dir / "public-results.tsv").open("w", newline="", encoding="utf-8") as handle:
    writer = csv.DictWriter(handle, fieldnames=fields, delimiter="\t", lineterminator="\n")
    writer.writeheader()
    for parser in parsers:
        with (out_dir / f"raw-{parser}.tsv").open(newline="", encoding="utf-8") as source:
            for row in csv.DictReader(source, delimiter="\t"):
                if Path(row["file"]).name == "TOTAL": continue
                per_document_ms = float(row["parse_ms"]) + float(row["count_ms"])
                iterations = int(row["iter"])
                size_mib = int(row["bytes"]) / (1024.0 * 1024.0)
                writer.writerow({
                    "parser": parser, "dataset": Path(row["file"]).name,
                    "input_bytes": row["bytes"], "iterations": row["iter"],
                    "best_ms": f"{per_document_ms * iterations:.3f}",
                    "mb_s": f"{size_mib / (per_document_ms / 1000.0):.1f}",
                    "checksum": row["nodes"],
                })
