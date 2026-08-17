# Examples

Run examples from the parser directory with `cargo run --example NAME -- [OPTIONS]`.

## Common tasks

- `quickstart`: parse, navigate, edit, query, and serialize a document.
- `build_document`: construct a document with the streaming builder.
- `xpath`: compile a namespace-aware XPath expression and bind a variable.
- `read_only`: compare validation, structural counting, a caller-buffer view, and a self-contained
  compact document.
- `parse_file`: load one or more XML files and print structural summaries.

For example:

```bash
cargo run --example quickstart
cargo run --example build_document
cargo run --example xpath
cargo run --example read_only
cargo run --example parse_file -- tests/fixtures/encoding/quick-xml/utf8.xml
```

## Diagnostics and benchmarks

- `encoding_probe`: summarize encoding support for a file or directory.
- `xmlts_probe`: run the external XML Test Suite corpus; use `--help` for corpus options.
- `attribute_scaling`: measure validation and parsing as attribute counts grow.
- `bench_parse`: compare the parser's DOM, compact, view, validation, count, and streaming modes.
- `bench_mutation`: measure parsing, querying, editing, construction, and serialization workloads.
- `bench_mutation_lifecycle`: exercise a large parse/edit/walk/serialize/clear lifecycle.
- `bench_compact_pugi`: compare compact-document and editable-DOM workloads using one input file.
- `bench_compact_decision`: run focused generated-input experiments for representation decisions.

Representative smoke commands are:

```bash
cargo run --example encoding_probe -- tests/fixtures/encoding
cargo run --example bench_parse -- \
  --runs 1 --iterations 1 tests/fixtures/encoding/quick-xml/utf8.xml
cargo run --example bench_mutation -- \
  --workload parse-walk --runs 1 --iterations 1 --min-duration-ms 1 \
  tests/fixtures/encoding/quick-xml/utf8.xml
cargo run --example bench_mutation_lifecycle -- --elements 100
cargo run --example bench_compact_pugi -- \
  --engine compact --workload parse --runs 1 --warmup 0 --iterations 1 \
  --min-duration-ms 1 tests/fixtures/encoding/quick-xml/utf8.xml
cargo run --example bench_compact_decision -- compact-parse gen:wide:10 1
```

The benchmark programs report measurements; they are not substitutes for the maintained public
benchmark workflow in the repository root.
