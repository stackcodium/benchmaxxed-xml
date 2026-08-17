# Running the XML benchmark

## Supported environment

The runner targets Linux and uses `/proc/self/status` plus GNU `time -v` for memory/process
metrics. The verified scopes are:

- `x86_64`: the complete six-parser configuration and included performance report.
- `aarch64`: native Benchmaxxed XML and pugixml correctness, compact/full traversal, mutation,
  XPath, serialization, latency, and peak-RSS validation recorded in
  [provenance/VERIFICATION.md](provenance/VERIFICATION.md).

Required tools:

- Rust and Cargo with edition 2021 support
- a C++17 compiler
- PHP `8.5.9` with FFI and the system `libxml2.so.2`
- Python `3.13.14` with the standard-library `ctypes` module and the system `libxml2.so.2`
- Git
- GNU time at `/usr/bin/time`
- Bun `1.4.0-canary.1+1dd66afde`

No reference source is downloaded implicitly by the benchmark command.

Release maintainers use the canonical suite runner to select the current Bun canary at publication
time, record the measured revision, and regenerate these version references. Its
`UPDATE_BUN=0` option is only for deliberately reproducing an already pinned historical run; the
public runner in this repository always checks the pinned version shown above.

## Fetch the pinned reference sources

```bash
scripts/fetch-references.sh
```

That script checks out:

- [quick-xml](https://github.com/tafia/quick-xml) at `4deda08abeffdc188c269360229cf47e12a77a9f`
- [pugixml](https://github.com/zeux/pugixml) at `27b68329de32cf9c601ca8eb6c588fd639960c40`
- [serde-rs/json-benchmark](https://github.com/serde-rs/json-benchmark) at `17b13dd2d7a5e5fdd5594e847077932f955b5e2b`

Bun, PHP, and Python must report the exact published runtime versions:

```bash
bun --revision
# 1.4.0-canary.1+1dd66afde
php -r 'echo PHP_VERSION, "\n";'
# 8.5.9
python3 -c 'import platform; print(platform.python_version())'
# 3.13.14
```

To intentionally benchmark other runtime versions, set `ALLOW_VERSION_MISMATCH=1`; the resulting
report must not be presented as the included published run without updating its provenance.

## Verify the package

```bash
python3 scripts/verify-package.py
```

This verifies required files, dataset hashes, reference metadata, forbidden local
paths, and the absence of build products or nested Git metadata.

## Smoke benchmark

```bash
RUNS=1 MIN_MS=25 WARMUP=0 COOLDOWN_SECONDS=0 scripts/run-benchmark.sh
```

## Full benchmark

Run on an otherwise idle machine:

```bash
scripts/run-benchmark.sh
```

Results are written below `.local/xml-parser-bench/`; the command does not overwrite the included
report under `reports/latest/`. Defaults can be overridden with `RUNS`, `MIN_MS`, `WARMUP`,
`ITERATIONS`, `COOLDOWN_SECONDS`, `BUN_BIN`, `PHP_BIN`, `PYTHON_BIN`, `CXX`, and `TIME_BIN`.

To validate only the native implementations on a machine where the pinned Bun build is unavailable:

```bash
PARSERS=benchmaxxed,pugixml,quickxml RUNS=1 MIN_MS=25 WARMUP=0 COOLDOWN_SECONDS=0 \
  scripts/run-benchmark.sh
```

`PARSERS` accepts `benchmaxxed`, `pugixml`, `quickxml`, `bun`, `php`, and `python`. A report that
omits an implementation is a compatibility/smoke report, not the published six-parser comparison.

For comparable results, retain the default parser modes and reference revisions. Record CPU
governor, background workload, thermals, and architecture when publishing measurements.

## Reproduce the converted datasets

The benchmark includes the converted XML files, so conversion is not part of timed runs. To
recreate them from the pinned source corpus in a temporary directory:

```bash
tmp_dir="$(mktemp -d)"
cargo run --release --manifest-path tools/xml-converters/Cargo.toml -- \
  convert-benchmark-json \
  --input-dir references/json-benchmark/data \
  --output-dir "$tmp_dir"
sha256sum "$tmp_dir"/*.xml
```

Compare the hashes with `provenance/datasets.toml`. Removing the temporary directory is left to the
caller so the regenerated files can be inspected first.
