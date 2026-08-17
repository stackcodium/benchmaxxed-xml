# Verification status

## Linux x86_64

- Full six-parser performance run on `x86_64`: Benchmaxxed XML, pugixml, quick-xml, Bun XML,
  PHP libxml2, and Python libxml2.
- Bun runtime: `1.4.0-canary.1+1dd66afde` (`1dd66afde213732c645c60ac08cf68f1087a271d`).
- PHP runtime: `8.5.9`; Python runtime: `3.13.14`.
- All implementations produced the same semantic node counts for all three datasets.
- Parser source: `7e3d7f924bd94d6aefb49d63f8fa81eb09e4021b` from a clean checkout.

## Linux aarch64

- Native Benchmaxxed XML and pugixml performance/parity run verified on `2026-08-17`.
- Architecture `aarch64`; Rust `1.97.1`, GCC `13.3.0`, Python
  `3.12.3`.
- Parser source: `7e3d7f924bd94d6aefb49d63f8fa81eb09e4021b`;
  pugixml source: `27b68329de32cf9c601ca8eb6c588fd639960c40`.
- Protocol: CPU `0`, `RUNS=7`, `MIN_MS=500`,
  `WARMUP=1`, `COOLDOWN_SECONDS=2`.
- Compact/full validation produced `84` result rows with zero losses at zero
  tolerance.
- Mutation/XPath validation matched `21/21` semantic
  outputs; Benchmaxxed XML speedups ranged from `1.138x` to `2.407x`
  over the compact pugixml reference for the measured targets.

The ARM evidence is a separate native comparison against pugixml. The included six-parser HTML
report is the x86_64 run described by `reports/latest/environment.json`; it does not claim Bun,
quick-xml, PHP, or Python validation on ARM.

## Publication gate

A publication must be built from a clean parser checkout at the configured revision. The strict
exporter and `scripts/verify-package.py` reject dirty parser provenance, mismatched references,
invalid dataset hashes, build products, nested Git metadata, and private local paths.
