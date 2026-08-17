#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${OUT_DIR:-$ROOT_DIR/.local/xml-parser-bench}"
RUNS="${RUNS:-7}"
MIN_MS="${MIN_MS:-500}"
WARMUP="${WARMUP:-1}"
ITERATIONS="${ITERATIONS:-1}"
TIME_BIN="${TIME_BIN:-/usr/bin/time}"
COOLDOWN_SECONDS="${COOLDOWN_SECONDS:-2}"
CXX="${CXX:-c++}"
BUN_BIN="${BUN_BIN:-$(command -v bun || true)}"
PHP_BIN="${PHP_BIN:-$(command -v php || true)}"
PYTHON_BIN="${PYTHON_BIN:-$(command -v python3 || true)}"
QUICKXML_DIR="$ROOT_DIR/references/quick-xml"
PUGIXML_DIR="$ROOT_DIR/references/pugixml"
QUICKXML_REVISION="4deda08abeffdc188c269360229cf47e12a77a9f"
PUGIXML_REVISION="27b68329de32cf9c601ca8eb6c588fd639960c40"
BUN_VERSION="1.4.0-canary.1+1dd66afde"
PHP_VERSION="8.5.9"
PYTHON_VERSION="3.13.14"
PARSERS="${PARSERS:-benchmaxxed,pugixml,quickxml,bun,php,python}"

fail() { echo "error: $*" >&2; exit 1; }
require_command() { command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"; }
require_revision() {
  local directory="$1" expected="$2" name="$3"
  [[ -d "$directory/.git" ]] || fail "missing $name checkout; run scripts/fetch-references.sh"
  local actual
  actual="$(git -C "$directory" rev-parse HEAD)"
  [[ "$actual" == "$expected" ]] || fail "$name revision mismatch: expected $expected, found $actual"
}
selected() { [[ ",$PARSERS," == *",$1,"* ]]; }

require_command cargo
require_command python3
require_command git
require_command "$CXX"
[[ -x "$TIME_BIN" ]] || fail "GNU time not found at $TIME_BIN"
if selected quickxml; then require_revision "$QUICKXML_DIR" "$QUICKXML_REVISION" quick-xml; fi
if selected pugixml; then require_revision "$PUGIXML_DIR" "$PUGIXML_REVISION" pugixml; fi

if selected bun; then
  [[ -n "$BUN_BIN" && -x "$BUN_BIN" ]] || fail "Bun is required; expected version $BUN_VERSION"
  ACTUAL_BUN_VERSION="$($BUN_BIN --revision 2>/dev/null || $BUN_BIN --version)"
  if [[ "$ACTUAL_BUN_VERSION" != "$BUN_VERSION" && "${ALLOW_VERSION_MISMATCH:-0}" != "1" ]]; then
    fail "Bun version mismatch: expected $BUN_VERSION, found $ACTUAL_BUN_VERSION"
  fi
fi
if selected php; then
  [[ -n "$PHP_BIN" && -x "$PHP_BIN" ]] || fail "PHP is required; expected version $PHP_VERSION"
  ACTUAL_PHP_VERSION="$($PHP_BIN -r 'echo PHP_VERSION;')"
  if [[ "$ACTUAL_PHP_VERSION" != "$PHP_VERSION" && "${ALLOW_VERSION_MISMATCH:-0}" != "1" ]]; then
    fail "PHP version mismatch: expected $PHP_VERSION, found $ACTUAL_PHP_VERSION"
  fi
  "$PHP_BIN" -d ffi.enable=true -r 'FFI::cdef("int xmlCheckVersion(int version);", "libxml2.so.2");' \
    || fail "PHP FFI and libxml2.so.2 are required"
fi
if selected python; then
  [[ -n "$PYTHON_BIN" && -x "$PYTHON_BIN" ]] || fail "Python is required; expected version $PYTHON_VERSION"
  ACTUAL_PYTHON_VERSION="$($PYTHON_BIN -c 'import platform; print(platform.python_version())')"
  if [[ "$ACTUAL_PYTHON_VERSION" != "$PYTHON_VERSION" && "${ALLOW_VERSION_MISMATCH:-0}" != "1" ]]; then
    fail "Python version mismatch: expected $PYTHON_VERSION, found $ACTUAL_PYTHON_VERSION"
  fi
  "$PYTHON_BIN" -c 'import ctypes; ctypes.CDLL("libxml2.so.2")' \
    || fail "Python ctypes and libxml2.so.2 are required"
fi

FILES=(
  "$ROOT_DIR/datasets/canada.xml"
  "$ROOT_DIR/datasets/citm_catalog.xml"
  "$ROOT_DIR/datasets/twitter.xml"
)
for file in "${FILES[@]}"; do [[ -f "$file" ]] || fail "missing dataset: $file"; done

mkdir -p "$OUT_DIR/bin"
if selected benchmaxxed; then cargo build --release --manifest-path "$ROOT_DIR/parser/Cargo.toml" --example bench_parse; fi
if selected quickxml; then cargo build --release --manifest-path "$ROOT_DIR/benchmarks/quickxml-bench/Cargo.toml"; fi
if selected pugixml; then
  "$CXX" -O3 -DNDEBUG -std=c++17 -DPUGIXML_COMPACT \
    -I "$PUGIXML_DIR/src" "$ROOT_DIR/benchmarks/pugixml_bench.cpp" \
    "$PUGIXML_DIR/src/pugixml.cpp" -o "$OUT_DIR/bin/pugixml_bench"
fi

run_timed() {
  local parser="$1"; shift
  local raw="$OUT_DIR/raw-${parser}.tsv" time_log="$OUT_DIR/time-${parser}.txt"
  "$TIME_BIN" -v -o "$time_log" "$@" > "$raw"
  local cpu rss_kb elapsed elapsed_seconds rss_mb
  cpu="$(awk -F': ' '/Percent of CPU this job got/ { gsub(/%/, "", $2); print $2 }' "$time_log")"
  rss_kb="$(awk -F': ' '/Maximum resident set size/ { print $2 }' "$time_log")"
  elapsed="$(awk -F': ' '/Elapsed/ { print $NF }' "$time_log")"
  elapsed_seconds="$(python3 - "$elapsed" <<'PY'
import sys
p=sys.argv[1].strip().split(":")
try:
    value=(int(p[0])*3600+int(p[1])*60+float(p[2])) if len(p)==3 else ((int(p[0])*60+float(p[1])) if len(p)==2 else float(p[0]))
    print(f"{value:.3f}")
except ValueError: print("0.000")
PY
)"
  rss_mb="$(python3 - "$rss_kb" <<'PY'
import sys
try: print(f"{int(sys.argv[1])/1024:.1f}")
except (ValueError, IndexError): print("0.0")
PY
)"
  printf '%s\t%s\t%s\t%s\n' "$parser" "${cpu:-0}" "$rss_mb" "$elapsed_seconds" >> "$OUT_DIR/process-metrics.tsv"
  sleep "$COOLDOWN_SECONDS"
}

printf 'parser\tcpu_percent\tmax_rss_mb\telapsed_seconds\n' > "$OUT_DIR/process-metrics.tsv"
REPORT_PARSERS=()
if selected benchmaxxed; then
  run_timed benchmaxxed-xml-walk "$ROOT_DIR/parser/target/release/examples/bench_parse" \
    --view-walk --compact-dom --trusted-xml-chars --trusted-references --trusted-attributes \
    --mode-label benchmaxxed-xml-walk --iterations "$ITERATIONS" --warmup "$WARMUP" \
    --runs "$RUNS" --min-duration-ms "$MIN_MS" "${FILES[@]}"
  REPORT_PARSERS+=(benchmaxxed-xml-walk)
fi
if selected pugixml; then
  run_timed pugixml-minimal-walk "$OUT_DIR/bin/pugixml_bench" --mode minimal \
    --iterations "$ITERATIONS" --warmup "$WARMUP" --runs "$RUNS" --min-duration-ms "$MIN_MS" "${FILES[@]}"
  REPORT_PARSERS+=(pugixml-minimal-walk)
fi
if selected quickxml; then
  run_timed quickxml-event-walk "$ROOT_DIR/benchmarks/quickxml-bench/target/release/quickxml-bench" --mode borrowed \
    --iterations "$ITERATIONS" --warmup "$WARMUP" --runs "$RUNS" --min-duration-ms "$MIN_MS" "${FILES[@]}"
  REPORT_PARSERS+=(quickxml-event-walk)
fi
if selected bun; then
  run_timed bun-xml-ordered-walk "$BUN_BIN" "$ROOT_DIR/benchmarks/bun-xml-bench.ts" \
    --iterations "$ITERATIONS" --warmup "$WARMUP" --runs "$RUNS" --min-duration-ms "$MIN_MS" "${FILES[@]}"
  REPORT_PARSERS+=(bun-xml-ordered-walk)
fi
if selected php; then
  run_timed php-libxml2-compact-walk "$PHP_BIN" -d ffi.enable=true "$ROOT_DIR/benchmarks/php-xml-bench.php" \
    --iterations "$ITERATIONS" --warmup "$WARMUP" --runs "$RUNS" --min-duration-ms "$MIN_MS" "${FILES[@]}"
  REPORT_PARSERS+=(php-libxml2-compact-walk)
fi
if selected python; then
  run_timed python-libxml2-compact-walk "$PYTHON_BIN" "$ROOT_DIR/benchmarks/python-xml-bench.py" \
    --iterations "$ITERATIONS" --warmup "$WARMUP" --runs "$RUNS" --min-duration-ms "$MIN_MS" "${FILES[@]}"
  REPORT_PARSERS+=(python-libxml2-compact-walk)
fi
[[ ${#REPORT_PARSERS[@]} -gt 0 ]] || fail "PARSERS selected no implementations"

python3 "$ROOT_DIR/scripts/normalize-results.py" "$OUT_DIR" "${REPORT_PARSERS[@]}"
cp "$OUT_DIR/process-metrics.tsv" "$OUT_DIR/public-process-metrics.tsv"
PARSER_CSV="$(IFS=,; echo "${REPORT_PARSERS[*]}")"
python3 "$ROOT_DIR/scripts/capture-environment.py" \
  --out "$OUT_DIR/environment.json" \
  --results "$OUT_DIR/public-results.tsv" \
  --metrics "$OUT_DIR/public-process-metrics.tsv" \
  --parser-dir "$ROOT_DIR/parser" \
  --suite-dir "$ROOT_DIR" \
  --quickxml-dir "$QUICKXML_DIR" \
  --pugixml-dir "$PUGIXML_DIR" \
  --dataset-source-dir "$ROOT_DIR/references/json-benchmark" \
  --bun-bin "$BUN_BIN" \
  --php-bin "$PHP_BIN" \
  --python-bin "$PYTHON_BIN" \
  --parsers "$PARSER_CSV" \
  --runs "$RUNS" \
  --minimum-duration-ms "$MIN_MS" \
  --warmup "$WARMUP" \
  --iterations "$ITERATIONS" \
  --cooldown-seconds "$COOLDOWN_SECONDS"
python3 "$ROOT_DIR/scripts/generate-parser-report.py" "$OUT_DIR/public-results.tsv" \
  --metrics "$OUT_DIR/public-process-metrics.tsv" \
  --out "$OUT_DIR/xml-parser-report.html"
echo "generated $OUT_DIR/xml-parser-report.html"
