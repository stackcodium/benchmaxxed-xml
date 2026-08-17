# Benchmaxxed XML

A dependency-free safe Rust XML parser with mutable `XmlDom`, compact read-only representations for
caller-provided buffers or self-contained documents, validation-only parsing, count-only parsing,
XPath, source offsets, and encoding-aware byte input/output.

## Example

```rust
use xml_parser::XmlDom;

let document = XmlDom::parse("<catalog><item id='1'>book</item></catalog>")?;
let catalog = document.root();
let item = catalog.child("item")?.ok_or("missing item")?;
assert_eq!(item.attribute("id")?.as_deref(), Some("1"));

let added = catalog.append_element("item")?;
added.set_attribute("id", "2")?;
added.set_text("pen")?;
assert_eq!(document.select_elements("//item")?.len(), 2);
# Ok::<(), Box<dyn std::error::Error>>(())
```

Use `parse_document_view` for a read-only tree over the caller's buffer and
`parse_compact_document` for a self-contained, `Send + Sync` read-only scan/index with scalar data
available by reference and copyable dense IDs. Prefer `XmlDom` for general navigation, mutation,
stable identities, decoded values, and XPath. Use
`validate_document` when only validity matters, and `count_document` when only structural counts
are needed. Byte-oriented variants perform XML encoding detection and decoding.

`XmlDomNode` handles have stable document-scoped IDs across index-shifting edits and moves;
removed subtrees report `DeletedHandle`. Nodes expose kind, snapshot, subtree, and inner-XML
operations. Ordinary parsing remains strict and atomic, while explicitly named tolerant
document/fragment APIs return a closed useful prefix with the original diagnostic. `Clone` creates
an independent copy, `share` creates an explicit same-thread mutable alias, and a document with no
aliases can cross a worker boundary through `XmlDomSend`.

## Development

```bash
cargo test --locked --offline
cargo check --all-targets --locked --offline
```

Runnable examples for common tasks, diagnostics, and benchmarks are cataloged in
[`examples/README.md`](examples/README.md).

The comparative benchmark, pinned reference implementations, datasets, methodology, and generated
reports are included in the repository root.

## License

MIT. See [LICENSE](LICENSE).
