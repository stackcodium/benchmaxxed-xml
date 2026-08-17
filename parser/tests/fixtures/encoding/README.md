# XML Encoding Fixtures

This directory vendors a compact, diverse set of XML byte-input fixtures so the
encoding tests do not depend on cloned upstream repositories under `.local/`.

Sources:

- `quick-xml/tests/documents/encoding`: the full focused encoding fixture set.
- `python-feedparser/feedparser/tests/encoding`: the full feedparser encoding
  stress set, including aliases, UTF-32, code pages, EBCDIC labels, mismatch
  cases, and invalid-byte cases.
- `xml-conformance-suite/packages/test-data/xmlconf`: selected W3C/XMLTS
  Japanese encoding, BOM, UTF-16, encoding-declaration, and IBM P04 fixtures.

Source license files are copied under `licenses/`.

Current expected probe shape:

```text
files=296
decoded=108
unsupported=160
decode_errors=28
encodings: utf8=53 ascii=0 utf16=26 utf32=10 latin1=19
```

The `decode_errors` files are intentional negative/adversarial cases. They
exercise invalid XML character handling and encoding mismatch behavior.
