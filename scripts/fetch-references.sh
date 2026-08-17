#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mkdir -p "$ROOT_DIR/references"

fetch() {
  local name="$1" url="$2" revision="$3" directory="$ROOT_DIR/references/$1"
  if [[ ! -d "$directory/.git" ]]; then git clone "$url" "$directory"; fi
  git -C "$directory" fetch --tags origin
  git -C "$directory" checkout --detach "$revision"
  [[ "$(git -C "$directory" rev-parse HEAD)" == "$revision" ]] || { echo "revision check failed for $name" >&2; exit 1; }
}

fetch quick-xml https://github.com/tafia/quick-xml 4deda08abeffdc188c269360229cf47e12a77a9f
fetch pugixml https://github.com/zeux/pugixml 27b68329de32cf9c601ca8eb6c588fd639960c40
fetch json-benchmark https://github.com/serde-rs/json-benchmark 17b13dd2d7a5e5fdd5594e847077932f955b5e2b
