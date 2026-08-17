#!/usr/bin/env python3
from __future__ import annotations

import argparse
import csv
import html
import platform
import re
import subprocess
from pathlib import Path


PARSER_COLORS = {
    "benchmaxxed-json": "bench",
    "serde_json": "serde",
    "bun-json": "bunjs",
    "bun-json-1.3.14": "bunjs",
    "bun-json-1.4.0": "violet",
    "php-json": "php",
    "python-json": "gold",
    "simdjson": "bun",
    "benchmaxxed-syml": "bench",
    "benchmaxxed-syml-walk": "bench",
    "saphyr-parser": "gold",
    "bun-yaml": "violet",
    "bun-yaml-walk": "violet",
    "benchmaxxed-toml-walk": "bench",
    "bun-toml-walk": "violet",
    "benchmaxxed-xml": "bench",
    "pugixml-minimal": "gold",
    "quickxml-borrowed": "serde",
    "benchmaxxed-xml-walk": "bench",
    "benchmaxxed-xml-owned-full-dom": "bench",
    "benchmaxxed-xml-compact-owned-full-dom": "violet",
    "pugixml-owned-full-dom": "gold",
    "pugixml": "gold",
    "local-json-retained-walk": "bunjs",
    "local-toml-retained-walk": "bench",
    "local-toml-retained-walk-before": "gold",
    "local-toml-retained-walk-after": "bench",
    "local-xml-retained-walk": "violet",
    "pugixml-minimal-walk": "gold",
    "quickxml-event-walk": "serde",
    "bun-xml-ordered-walk": "violet",
    "php-libxml2-compact-walk": "php",
    "python-libxml2-compact-walk": "coral",
}

PARSER_LABELS = {
    "benchmaxxed-json": "benchmaxxed-json",
    "serde_json": "serde_json",
    "bun-json": "Bun JSON.parse",
    "bun-json-1.3.14": "Bun 1.3.14 JSON.parse",
    "bun-json-1.4.0": "Bun 1.4.0 JSON.parse",
    "php-json": "PHP json_decode",
    "python-json": "Python json.loads",
    "simdjson": "C++ simdjson",
    "benchmaxxed-syml": "benchmaxxed-syml",
    "benchmaxxed-syml-walk": "benchmaxxed SYML walk",
    "saphyr-parser": "Saphyr parser",
    "bun-yaml": "Bun YAML",
    "bun-yaml-walk": "Bun YAML.parse walk",
    "benchmaxxed-toml-walk": "benchmaxxed TOML walk",
    "bun-toml-walk": "Bun TOML.parse walk",
    "benchmaxxed-xml": "benchmaxxed-xml",
    "pugixml-minimal": "pugixml minimal",
    "quickxml-borrowed": "quick-xml borrowed",
    "benchmaxxed-xml-walk": "benchmaxxed XML walk",
    "benchmaxxed-xml-owned-full-dom": "benchmaxxed XmlDom full walk",
    "benchmaxxed-xml-compact-owned-full-dom": "benchmaxxed compact owned walk",
    "pugixml-owned-full-dom": "pugixml full DOM walk",
    "pugixml": "pugixml",
    "local-json-retained-walk": "benchmaxxed JSON retained walk",
    "local-toml-retained-walk": "benchmaxxed TOML retained walk",
    "local-toml-retained-walk-before": "TOML before",
    "local-toml-retained-walk-after": "TOML optimized",
    "local-xml-retained-walk": "benchmaxxed XML retained walk",
    "pugixml-minimal-walk": "pugixml minimal walk",
    "quickxml-event-walk": "quick-xml event walk",
    "bun-xml-ordered-walk": "Bun XML.parse ordered walk",
    "php-libxml2-compact-walk": "PHP libxml2 compact walk",
    "python-libxml2-compact-walk": "Python libxml2 compact walk",
}

DATASET_LABELS = {
    "canada.json": "CANADA",
    "twitter.json": "TWITTER",
    "citm_catalog.json": "CITM",
    "canada.xml": "CANADA",
    "twitter.xml": "TWITTER",
    "citm_catalog.xml": "CITM",
    "canada.yaml": "CANADA",
    "twitter.yaml": "TWITTER",
    "citm_catalog.yaml": "CITM",
    "canada.toml": "CANADA",
    "twitter.toml": "TWITTER",
    "citm_catalog.toml": "CITM",
}

DATASET_ORDER = [
    "canada.json",
    "canada.xml",
    "canada.yaml",
    "canada.toml",
    "twitter.json",
    "twitter.xml",
    "twitter.yaml",
    "twitter.toml",
    "citm_catalog.json",
    "citm_catalog.xml",
    "citm_catalog.yaml",
    "citm_catalog.toml",
]


def parse_float(value: str | None) -> float | None:
    try:
        return float(value or "")
    except ValueError:
        return None


def read_rows(path: Path) -> list[dict[str, str]]:
    with path.open(newline="", encoding="utf-8") as handle:
        rows = list(csv.DictReader(handle, delimiter="\t"))
    required = {"parser", "dataset", "input_bytes", "iterations", "best_ms", "mb_s", "checksum"}
    missing = required.difference(rows[0].keys() if rows else set())
    if missing:
        raise SystemExit(f"{path} is missing columns: {', '.join(sorted(missing))}")
    return rows


def read_metrics(path: Path | None) -> dict[str, dict[str, float]]:
    if path is None or not path.exists():
        return {}
    with path.open(newline="", encoding="utf-8") as handle:
        reader = csv.DictReader(handle, delimiter="\t")
        return {
            row["parser"]: {
                "cpu_percent": parse_float(row.get("cpu_percent")) or 0.0,
                "max_rss_mb": parse_float(row.get("max_rss_mb")) or 0.0,
                "elapsed_seconds": parse_float(row.get("elapsed_seconds")) or 0.0,
            }
            for row in reader
            if row.get("parser")
        }


def dataset_sort_key(dataset: str) -> tuple[int, str]:
    try:
        return (DATASET_ORDER.index(dataset), dataset)
    except ValueError:
        return (99, dataset)


def parser_sort_key(parser: str) -> tuple[int, str]:
    order = {
        "simdjson": 0,
        "bun-json": 1,
        "bun-json-1.3.14": 1,
        "bun-json-1.4.0": 2,
        "php-json": 3,
        "python-json": 4,
        "serde_json": 5,
        "benchmaxxed-json": 6,
        "saphyr-parser": 7,
        "bun-yaml": 8,
        "benchmaxxed-syml": 9,
        "bun-yaml-walk": 10,
        "benchmaxxed-syml-walk": 11,
        "bun-toml-walk": 12,
        "benchmaxxed-toml-walk": 13,
        "pugixml-minimal": 14,
        "quickxml-borrowed": 15,
        "benchmaxxed-xml": 16,
        "benchmaxxed-xml-walk": 17,
        "pugixml-minimal-walk": 18,
        "quickxml-event-walk": 19,
        "bun-xml-ordered-walk": 20,
        "php-libxml2-compact-walk": 21,
        "python-libxml2-compact-walk": 22,
        "benchmaxxed-xml-owned-full-dom": 21,
        "benchmaxxed-xml-compact-owned-full-dom": 22,
        "pugixml-owned-full-dom": 23,
        "local-json-retained-walk": 21,
        "local-toml-retained-walk": 22,
        "local-toml-retained-walk-before": 22,
        "local-toml-retained-walk-after": 23,
        "local-xml-retained-walk": 24,
    }
    return (order.get(parser, 99), parser)


def grouped_rows(rows: list[dict[str, str]]) -> dict[str, dict[str, dict[str, str]]]:
    grouped: dict[str, dict[str, dict[str, str]]] = {}
    for row in rows:
        grouped.setdefault(row["dataset"], {})[row["parser"]] = row
    return dict(sorted(grouped.items(), key=lambda item: dataset_sort_key(item[0])))


def format_bar_value(value: float, unit: str) -> str:
    if value >= 1000:
        return f"{value / 1000:.2f}k"
    if value >= 100:
        return f"{value:.0f}"
    if unit == "us":
        return f"{value:.0f}"
    return f"{value:.1f}"


def nice_axis(max_value: float, unit: str) -> list[str]:
    if max_value <= 0:
        return ["0", "0", "0", "0", "0"]
    return [format_bar_value(max_value * factor, unit) for factor in (1.0, 0.75, 0.5, 0.25, 0.0)]


def pct(value: float, max_value: float) -> float:
    if max_value <= 0:
        return 0.0
    return max(1.0, min(100.0, value / max_value * 100.0))


def row_value(row: dict[str, str], field: str) -> float:
    return parse_float(row.get(field)) or 0.0


def best_parse_us(row: dict[str, str]) -> float:
    iterations = row_value(row, "iterations")
    best_ms = row_value(row, "best_ms")
    if iterations <= 0:
        return 0.0
    return best_ms * 1000.0 / iterations


def bar_grid_style(count: int) -> str:
    columns = max(1, count)
    max_width = 34 if columns <= 5 else 30
    min_width = 14 if columns <= 5 else 11
    return f"grid-template-columns: repeat({columns}, minmax({min_width}px, {max_width}px));"


def chart(
    title: str,
    icon: str,
    hint: str,
    datasets: dict[str, dict[str, dict[str, str]]],
    value_fn,
    unit: str,
    dark: bool = False,
) -> str:
    values = [
        value_fn(row)
        for parser_rows in datasets.values()
        for row in parser_rows.values()
    ]
    max_value = max(values, default=1.0) * 1.04
    parser_count = max((len(parser_rows) for parser_rows in datasets.values()), default=1)
    grid_style = html.escape(bar_grid_style(parser_count), quote=True)
    parts = [f'<article class="card{" dark" if dark else ""}" aria-label="{html.escape(title)} chart">']
    parts.append('<div class="card-head">')
    parts.append(f"<h2><span class=\"icon\">{html.escape(icon)}</span> {html.escape(title)}</h2>")
    parts.append(f'<span class="hint">{html.escape(hint)}</span></div>')
    parts.append('<div class="plot">')
    parts.append("<div class=\"yaxis\">" + "".join(f"<span>{html.escape(label)}</span>" for label in nice_axis(max_value, unit)) + "</div>")
    parts.append('<div class="plot-area">')
    for dataset, parser_rows in datasets.items():
        parts.append(f'<div class="group" style="{grid_style}">')
        for parser in sorted(parser_rows, key=parser_sort_key):
            value = value_fn(parser_rows[parser])
            parser_class = PARSER_COLORS.get(parser, "bench")
            parts.append(
                f'<div class="bar {html.escape(parser_class)}" style="height:{pct(value, max_value):.1f}%">'
                f'<span class="bar-label">{html.escape(format_bar_value(value, unit))}</span></div>'
            )
        parts.append("</div>")
    parts.append("</div>")
    parts.append('<div class="xlabels">' + "".join(f"<span>{html.escape(DATASET_LABELS.get(dataset, dataset))}</span>" for dataset in datasets) + "</div>")
    parts.append("</div></article>")
    return "\n".join(parts)


def metric_chart(title: str, icon: str, hint: str, metric_rows: dict[str, dict[str, float]], field: str, unit: str) -> str:
    values = [row.get(field, 0.0) for row in metric_rows.values()]
    max_value = max(values, default=1.0) * 1.08
    grid_style = html.escape(bar_grid_style(len(metric_rows)), quote=True)
    parts = [f'<article class="card dark" aria-label="{html.escape(title)} chart">']
    parts.append('<div class="card-head">')
    parts.append(f"<h2><span class=\"icon\">{html.escape(icon)}</span> {html.escape(title)}</h2>")
    parts.append(f'<span class="hint">{html.escape(hint)}</span></div>')
    parts.append('<div class="plot single">')
    parts.append("<div class=\"yaxis\">" + "".join(f"<span>{html.escape(format_bar_value(max_value * factor, unit))}</span>" for factor in (1.0, 0.75, 0.5, 0.25, 0.0)) + "</div>")
    parts.append('<div class="plot-area">')
    parts.append(f'<div class="group metric-group" style="{grid_style}">')
    for parser in sorted(metric_rows, key=parser_sort_key):
        value = metric_rows[parser].get(field, 0.0)
        parser_class = PARSER_COLORS.get(parser, "bench")
        suffix = "%" if unit == "pct" else ""
        parts.append(
            f'<div class="bar {html.escape(parser_class)}" style="height:{pct(value, max_value):.1f}%">'
            f'<span class="bar-label">{html.escape(format_bar_value(value, unit))}{suffix}</span></div>'
        )
    parts.append("</div></div>")
    parts.append('<div class="xlabels"><span>CANADA + TWITTER + CITM</span></div>')
    parts.append("</div></article>")
    return "\n".join(parts)


def system_note(metrics: dict[str, dict[str, float]]) -> str:
    system = platform.system()
    try:
        for line in Path("/etc/os-release").read_text(encoding="utf-8").splitlines():
            if line.startswith("NAME="):
                system = line.split("=", 1)[1].strip().strip('"')
                break
    except OSError:
        pass
    cpu = "unknown CPU"
    try:
        with Path("/proc/cpuinfo").open(encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("model name"):
                    cpu = line.split(":", 1)[1].strip()
                    break
    except OSError:
        pass
    if cpu == "unknown CPU":
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
    if cpu == "unknown CPU":
        cpu = platform.processor() or platform.machine() or cpu
    ram = "unknown RAM"
    try:
        with Path("/proc/meminfo").open(encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("MemTotal:"):
                    gib = int(line.split()[1]) / 1024 / 1024
                    ram = f"{gib:.0f} GiB RAM"
                    break
    except OSError:
        pass
    metric_text = "CPU/RSS measured with /usr/bin/time -v over separate full three-file parser-only benchmark commands."
    if not metrics:
        metric_text = "CPU/RSS metrics were not supplied."
    return f"System: {system} · {cpu} · {ram} · {metric_text}"


def methodology_note(parsers: list[str]) -> str:
    notes = []
    xml_modes = {
        "benchmaxxed-xml-walk": "Benchmaxxed uses its trusted compact view",
        "pugixml-minimal-walk": "pugixml uses minimal mode",
        "quickxml-event-walk": "quick-xml uses borrowed events",
        "bun-xml-ordered-walk": "Bun uses XML.parse({ compact: false })",
        "php-libxml2-compact-walk": "PHP uses libxml2 through FFI in compact mode",
        "python-libxml2-compact-walk": "Python uses libxml2 through ctypes in compact mode",
    }
    selected_xml_modes = [description for parser, description in xml_modes.items() if parser in parsers]
    if selected_xml_modes:
        notes.append(
            "XML throughput includes parse + complete walk; "
            + "; ".join(selected_xml_modes)
            + "."
        )
    if "bun-yaml-walk" in parsers:
        notes.append(
            "YAML throughput includes parse + complete value-tree walk; Bun uses YAML.parse and "
            "benchmaxxed uses its compact borrowed SYML document."
        )
    if "bun-toml-walk" in parsers:
        notes.append(
            "TOML throughput includes parse + complete value-tree walk; Bun uses TOML.parse and "
            "benchmaxxed uses its compact TOML document."
        )
    return " " + " ".join(notes) if notes else ""


def build_report(
    rows: list[dict[str, str]],
    metrics: dict[str, dict[str, float]],
    out: Path,
    source: Path,
    extra_note: str = "",
) -> None:
    datasets = grouped_rows(rows)
    parser_count = len({row["parser"] for row in rows})
    wide_primary_charts = parser_count > 3
    css = """
:root {
  --paper: #f2e8d2;
  --ink: #111111;
  --panel: #211f2b;
  --panel2: #302b39;
  --line: #111111;
  --cream: #fff8e8;
  --muted: #70675b;
  --bun: #ef5a4f;
  --serde: #4f7de8;
  --bunjs: #f2c94c;
  --php: #777bb4;
  --bench: #20ba74;
  --gold: #efb342;
  --violet: #8b5cf6;
  --coral: #e76f51;
}
* { box-sizing: border-box; }
body {
  margin: 0;
  min-height: 100vh;
  color: var(--ink);
  background:
    linear-gradient(90deg, rgba(17, 17, 17, 0.07) 1px, transparent 1px),
    linear-gradient(0deg, rgba(17, 17, 17, 0.07) 1px, transparent 1px),
    var(--paper);
  background-size: 12px 12px;
  font-family: "Courier New", Courier, monospace;
  letter-spacing: 0;
}
main { width: min(1080px, calc(100vw - 24px)); margin: 0 auto; padding: 12px 0 18px; }
.legend { display: flex; flex-wrap: wrap; gap: 10px; margin: 0 0 16px; font-size: 13px; font-weight: 700; }
.legend span {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  padding: 6px 8px;
  background: var(--cream);
  border: 3px solid var(--line);
  box-shadow: 3px 3px 0 #000;
}
.swatch { width: 18px; height: 18px; background: var(--bench); border: 2px solid var(--line); }
.swatch.bun { background: var(--bun); }
.swatch.serde { background: var(--serde); }
.swatch.bunjs { background: var(--bunjs); }
.swatch.php { background: var(--php); }
.swatch.gold { background: var(--gold); }
.swatch.violet { background: var(--violet); }
.swatch.coral { background: var(--coral); }
.charts { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 16px; }
.card.wide { grid-column: 1 / -1; }
.card {
  padding: 14px;
  background: var(--cream);
  border: 4px solid var(--line);
  box-shadow: 6px 6px 0 #000;
}
.card.dark { color: var(--cream); background: var(--panel); }
.card-head { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-bottom: 14px; }
h2 { display: flex; align-items: center; gap: 8px; margin: 0; font-size: 18px; line-height: 1.1; text-transform: uppercase; }
.icon { display: inline-block; width: 18px; text-align: center; }
.hint { color: var(--muted); font-size: 12px; text-align: right; white-space: nowrap; }
.dark .hint { color: #cbbfa8; }
.plot { display: grid; grid-template-columns: 72px 1fr; gap: 10px; align-items: end; min-height: 286px; }
.plot.single .plot-area { grid-template-columns: 1fr; }
.yaxis {
  height: 240px;
  display: flex;
  flex-direction: column;
  justify-content: space-between;
  color: var(--muted);
  font-size: 11px;
  text-align: right;
  padding-top: 2px;
  padding-bottom: 2px;
}
.dark .yaxis { color: #cbbfa8; }
.plot-area {
  height: 240px;
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 20px;
  align-items: end;
  padding: 10px 12px 0;
  background:
    linear-gradient(0deg, rgba(17, 17, 17, 0.13) 3px, transparent 3px),
    linear-gradient(90deg, rgba(17, 17, 17, 0.08) 1px, transparent 1px),
    #e6dac2;
  background-size: 100% 48px, 12px 100%;
  border: 4px solid var(--line);
  border-bottom-width: 5px;
}
.dark .plot-area {
  background:
    linear-gradient(0deg, rgba(255, 248, 232, 0.16) 3px, transparent 3px),
    linear-gradient(90deg, rgba(255, 248, 232, 0.08) 1px, transparent 1px),
    var(--panel2);
}
.group {
  height: 100%;
  display: grid;
  gap: 8px;
  justify-content: center;
  align-items: end;
  position: relative;
}
.metric-group { max-width: 230px; margin: 0 auto; }
.bar {
  position: relative;
  min-height: 4px;
  background: var(--bench);
  border: 3px solid var(--line);
  box-shadow: inset 0 -7px 0 rgba(0, 0, 0, 0.18);
}
.bar.bun { background: var(--bun); }
.bar.serde { background: var(--serde); }
.bar.bunjs { background: var(--bunjs); }
.bar.php { background: var(--php); }
.bar.gold { background: var(--gold); }
.bar.violet { background: var(--violet); }
.bar.coral { background: var(--coral); }
.bar-label {
  position: absolute;
  left: 50%;
  bottom: calc(100% + 6px);
  transform: translateX(-50%) rotate(180deg);
  writing-mode: vertical-rl;
  padding: 5px 3px;
  color: var(--cream);
  background: var(--ink);
  border: 2px solid var(--line);
  font-size: 10px;
  line-height: 1;
  font-weight: 700;
  white-space: normal;
}
.xlabels {
  grid-column: 2;
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 20px;
  margin-top: 8px;
  font-size: 12px;
  font-weight: 700;
  text-align: center;
  text-transform: uppercase;
}
.single .xlabels { grid-template-columns: 1fr; }
.note {
  margin-top: 16px;
  padding: 9px 10px;
  color: #403a34;
  background: rgba(255, 248, 232, 0.82);
  border: 3px solid var(--line);
  box-shadow: 4px 4px 0 #000;
  font-size: 12px;
  line-height: 1.35;
}
@media (max-width: 860px) { .charts { grid-template-columns: 1fr; } }
@media (max-width: 640px) {
  .plot { grid-template-columns: 52px 1fr; }
  .plot-area { gap: 12px; padding-left: 8px; padding-right: 8px; }
  .group { gap: 6px; }
  .bar-label { font-size: 10px; }
}
"""
    throughput_chart = chart(
        "Throughput",
        "⚡",
        "MB/sec · higher better",
        datasets,
        lambda row: row_value(row, "mb_s"),
        "mb",
    )
    if wide_primary_charts:
        throughput_chart = throughput_chart.replace('<article class="card"', '<article class="card wide"', 1)
    primary_charts = [throughput_chart]
    time_chart = chart(
        "Best Time",
        "⏱",
        "microseconds · lower better",
        datasets,
        best_parse_us,
        "us",
    )
    if wide_primary_charts:
        time_chart = time_chart.replace('<article class="card"', '<article class="card wide"', 1)
    primary_charts.append(time_chart)
    parser_names = sorted({row["parser"] for row in rows}, key=parser_sort_key)
    legend = [
        f'<span><i class="swatch {html.escape(PARSER_COLORS.get(parser, ""))}"></i>{html.escape(PARSER_LABELS.get(parser, parser))}</span>'
        for parser in parser_names
    ]
    title = " vs ".join(PARSER_LABELS.get(parser, parser) for parser in parser_names)
    parts = [
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n",
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n",
        f"<title>{html.escape(title)}</title>\n",
        f"<style>{css}</style>\n</head>\n<body>\n<main>\n",
        '<div class="legend" aria-label="Legend">',
        *legend,
        "</div>\n",
        '<section class="charts">\n',
        *primary_charts,
    ]
    if metrics:
        parts.append(metric_chart("CPU", "▣", "whole benchmark command", metrics, "cpu_percent", "pct"))
        parts.append(metric_chart("RSS", "▥", "MB · lower better", metrics, "max_rss_mb", "mb"))
    parts.extend([
        "</section>\n",
        f'<p class="note">{html.escape(system_note(metrics) + methodology_note(parser_names) + (" " + extra_note if extra_note else ""))}</p>\n',
        "</main>\n</body>\n</html>\n",
    ])
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("".join(parts), encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description="Generate a pixel-style parser benchmark HTML report from TSV data.")
    parser.add_argument("results", type=Path, help="Parser benchmark results TSV")
    parser.add_argument("--metrics", type=Path, default=None, help="Optional parser CPU/RSS TSV")
    parser.add_argument("--out", type=Path, default=None, help="Output HTML path")
    parser.add_argument("--note", default="", help="Additional methodology or limitation note")
    args = parser.parse_args()
    rows = read_rows(args.results)
    metrics = read_metrics(args.metrics)
    out = args.out or args.results.with_name("json-parser-pixel-report.html")
    build_report(rows, metrics, out, args.results, args.note)
    print(f"generated {out}")


if __name__ == "__main__":
    main()
