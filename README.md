# Benchmaxxed XML Parser Benchmark

XML parser throughput and memory comparison with complete document walks.

[![Release](https://img.shields.io/github/v/release/stackcodium/benchmaxxed-xml)](https://github.com/stackcodium/benchmaxxed-xml/releases)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Package version: **v0.1.0**

[Benchmaxxed XML](parser/) is a Rust XML parser with compact document storage, mutable DOM APIs,
serialization, and XPath support. This repository includes the parser, comparison harnesses,
benchmark datasets, and a captured complete-walk performance report.

## Included benchmark report

- **[Open the latest complete-walk HTML report](https://stackcodium.github.io/benchmaxxed-xml/reports/latest/xml-parser-report.html)**

Included throughput in MB/s (higher is better):

| Parser | canada.xml | citm_catalog.xml | twitter.xml |
|---|---:|---:|---:|
| Benchmaxxed XML | 1656.3 | 1487.4 | 1152.3 |
| pugixml | 984.6 | 900.5 | 750.9 |
| quick-xml | 626.2 | 563.7 | 522.8 |
| Bun XML | 145.2 | 128.1 | 145.3 |
| PHP libxml2 | 113.8 | 86.2 | 83.5 |
| Python libxml2 | 60.9 | 54.6 | 49.6 |

These values come from the captured machine in `reports/latest/environment.json`; compare results
within this run, not across unrelated hosts.

- [Machine-readable results](reports/latest/results.tsv)
- [Process metrics](reports/latest/process-metrics.tsv)
- [Captured environment](reports/latest/environment.json)
- [Compatibility verification](provenance/VERIFICATION.md)

## Quick start

The native three-parser smoke test works without Bun:

```bash
git clone https://github.com/stackcodium/benchmaxxed-xml.git
cd benchmaxxed-xml
python3 scripts/verify-package.py
scripts/fetch-references.sh
PARSERS=benchmaxxed,pugixml,quickxml RUNS=1 MIN_MS=25 WARMUP=0 COOLDOWN_SECONDS=0 \
  scripts/run-benchmark.sh
```

For the complete six-parser run, including the pinned Bun, PHP, and Python runtimes, see
[RUNNING.md](RUNNING.md).

## Compared implementations

| Implementation | Recorded version or revision | Source |
|---|---|---|
| Benchmaxxed XML | `7e3d7f924bd94d6aefb49d63f8fa81eb09e4021b` (clean source) | [Included source](parser/) |
| quick-xml | `v0.41.0` / `4deda08abeffdc188c269360229cf47e12a77a9f` | [GitHub](https://github.com/tafia/quick-xml) |
| pugixml | `v1.16-1-g27b6832` / `27b68329de32cf9c601ca8eb6c588fd639960c40` | [GitHub](https://github.com/zeux/pugixml) |
| Bun XML | `1.4.0-canary.1+1dd66afde` / `1dd66afde213732c645c60ac08cf68f1087a271d` | [GitHub](https://github.com/oven-sh/bun) |
| PHP libxml2 | `8.5.9` | Installed runtime |
| Python libxml2 | `3.13.14` | Installed runtime |

Reference repositories are not vendored. `scripts/fetch-references.sh` clones and checks out the
exact revisions above. Bun, PHP, and Python are installed separately, and their runtime versions are
checked before the included configuration runs. Publication additionally requires a clean parser
checkout and passing strict export and package verification.

## Methodology

All measurements include parsing followed by a complete document walk. The walk counts elements,
attributes, and semantic nodes so an implementation cannot win by lazily skipping document data.
Whitespace-only text is omitted consistently. Entity-separated quick-xml text events are coalesced
into one semantic text node before counting.

Benchmaxxed uses the compact borrowed document view with the trusted-input switches shown in
`scripts/run-benchmark.sh`. pugixml uses `parse_minimal`; quick-xml uses borrowed events; Bun uses
`XML.parse(input, { compact: false })` followed by a JavaScript tree walk. PHP 8.5.9 calls the
installed libxml2 parser through FFI in compact mode, and Python 3.13.14 calls that same system
libxml2 parser through the standard-library `ctypes` module with the same parse flags. These are
deliberately documented operating modes, not claims that every parser exposes identical validation
behavior.

See [RUNNING.md](RUNNING.md) for requirements, reference checkout instructions, smoke tests, and
full benchmark commands.

## Datasets and licensing

The XML datasets are deterministic conversions of `canada.json`, `citm_catalog.json`, and
`twitter.json` from [serde-rs/json-benchmark](https://github.com/serde-rs/json-benchmark) at commit
`17b13dd2d7a5e5fdd5594e847077932f955b5e2b`. Dataset hashes and conversion provenance are recorded in
`provenance/datasets.toml`.

The benchmark package and included parser source are licensed under MIT. Dataset source license
copies and third-party notices are under `LICENSES/`.
