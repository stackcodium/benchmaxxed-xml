use std::{borrow::Cow, char};

use crate::{
    error::{XmlError, XmlErrorKind, XmlResult},
    syntax::is_space,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// The supported `XmlInputEncoding` alternatives.
pub enum XmlInputEncoding {
    /// Indicates `Utf8`.
    Utf8,
    /// Indicates `UsAscii`.
    UsAscii,
    /// Indicates `Utf16Le`.
    Utf16Le,
    /// Indicates `Utf16Be`.
    Utf16Be,
    /// Indicates `Utf32Le`.
    Utf32Le,
    /// Indicates `Utf32Be`.
    Utf32Be,
    /// Indicates `Latin1`.
    Latin1,
}

#[derive(Debug)]
/// A public `DecodedXml` value in the XML data model.
pub struct DecodedXml<'a> {
    /// The input.
    pub input: Cow<'a, str>,
    /// The encoding.
    pub encoding: XmlInputEncoding,
}

impl DecodedXml<'_> {
    /// Returns this value as str when it has that kind.
    pub fn as_str(&self) -> &str {
        &self.input
    }
}

/// Decodes xml bytes.
pub fn decode_xml_bytes(bytes: &[u8]) -> XmlResult<DecodedXml<'_>> {
    let detection = detect_encoding(bytes)?;
    let input = match detection.encoding {
        XmlInputEncoding::Utf8 if detection.skip_bytes == 0 => decode_utf8(bytes, 0)?,
        XmlInputEncoding::Utf8 => {
            let input = decode_utf8(bytes, detection.skip_bytes)?;
            validate_declared_encoding(input.as_bytes(), detection.encoding)?;
            input
        }
        XmlInputEncoding::UsAscii => decode_ascii(bytes, detection.skip_bytes)?,
        XmlInputEncoding::Utf16Le => {
            let input = decode_utf16(bytes, detection.skip_bytes, Endian::Little)?;
            validate_declared_encoding(input.as_bytes(), detection.encoding)?;
            Cow::Owned(input)
        }
        XmlInputEncoding::Utf16Be => {
            let input = decode_utf16(bytes, detection.skip_bytes, Endian::Big)?;
            validate_declared_encoding(input.as_bytes(), detection.encoding)?;
            Cow::Owned(input)
        }
        XmlInputEncoding::Utf32Le => {
            let input = decode_utf32(bytes, detection.skip_bytes, Endian::Little)?;
            validate_declared_encoding(input.as_bytes(), detection.encoding)?;
            Cow::Owned(input)
        }
        XmlInputEncoding::Utf32Be => {
            let input = decode_utf32(bytes, detection.skip_bytes, Endian::Big)?;
            validate_declared_encoding(input.as_bytes(), detection.encoding)?;
            Cow::Owned(input)
        }
        XmlInputEncoding::Latin1 => Cow::Owned(decode_latin1(bytes, detection.skip_bytes)),
    };

    Ok(DecodedXml {
        input,
        encoding: detection.encoding,
    })
}

impl DecodedXml<'_> {
    #[cold]
    #[inline(never)]
    pub(crate) fn translate_error(&self, source: &[u8], mut error: XmlError) -> XmlError {
        let mut decoded_offset = error.byte.min(self.as_str().len());
        while !self.as_str().is_char_boundary(decoded_offset) {
            decoded_offset -= 1;
        }
        let prefix = &self.as_str()[..decoded_offset];
        let source_len = match self.encoding {
            XmlInputEncoding::Utf8 | XmlInputEncoding::UsAscii => decoded_offset,
            XmlInputEncoding::Latin1 => prefix.chars().count(),
            XmlInputEncoding::Utf16Le | XmlInputEncoding::Utf16Be => {
                prefix.chars().map(char::len_utf16).sum::<usize>() * 2
            }
            XmlInputEncoding::Utf32Le | XmlInputEncoding::Utf32Be => prefix.chars().count() * 4,
        };
        let skip_bytes = match self.encoding {
            XmlInputEncoding::Utf8 if source.starts_with(b"\xef\xbb\xbf") => 3,
            XmlInputEncoding::Utf16Le if source.starts_with(&[0xff, 0xfe]) => 2,
            XmlInputEncoding::Utf16Be if source.starts_with(&[0xfe, 0xff]) => 2,
            XmlInputEncoding::Utf32Le if source.starts_with(&[0xff, 0xfe, 0, 0]) => 4,
            XmlInputEncoding::Utf32Be if source.starts_with(&[0, 0, 0xfe, 0xff]) => 4,
            _ => 0,
        };
        error.byte = skip_bytes + source_len;
        error
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Endian {
    Little,
    Big,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EncodingDetection {
    encoding: XmlInputEncoding,
    skip_bytes: usize,
}

fn detect_encoding(bytes: &[u8]) -> XmlResult<EncodingDetection> {
    if bytes.starts_with(b"\xef\xbb\xbf") {
        if bytes[3..].starts_with(b"\xef\xbb\xbf") {
            return Err(repeated_bom_error(3));
        }
        return Ok(EncodingDetection {
            encoding: XmlInputEncoding::Utf8,
            skip_bytes: 3,
        });
    }
    if bytes.starts_with(&[0x00, 0x00, 0xfe, 0xff]) {
        if bytes[4..].starts_with(&[0x00, 0x00, 0xfe, 0xff]) {
            return Err(repeated_bom_error(4));
        }
        return Ok(EncodingDetection {
            encoding: XmlInputEncoding::Utf32Be,
            skip_bytes: 4,
        });
    }
    if bytes.starts_with(&[0xff, 0xfe, 0x00, 0x00]) {
        if bytes[4..].starts_with(&[0xff, 0xfe, 0x00, 0x00]) {
            return Err(repeated_bom_error(4));
        }
        return Ok(EncodingDetection {
            encoding: XmlInputEncoding::Utf32Le,
            skip_bytes: 4,
        });
    }
    if bytes.starts_with(&[0xfe, 0xff]) {
        if bytes[2..].starts_with(&[0xfe, 0xff]) {
            return Err(repeated_bom_error(2));
        }
        return Ok(EncodingDetection {
            encoding: XmlInputEncoding::Utf16Be,
            skip_bytes: 2,
        });
    }
    if bytes.starts_with(&[0xff, 0xfe]) {
        if bytes[2..].starts_with(&[0xff, 0xfe]) {
            return Err(repeated_bom_error(2));
        }
        return Ok(EncodingDetection {
            encoding: XmlInputEncoding::Utf16Le,
            skip_bytes: 2,
        });
    }

    if matches!(bytes.get(..4), Some([0x00, 0x00, 0x00, b'<'])) {
        return Ok(EncodingDetection {
            encoding: XmlInputEncoding::Utf32Be,
            skip_bytes: 0,
        });
    }
    if matches!(bytes.get(..4), Some([b'<', 0x00, 0x00, 0x00])) {
        return Ok(EncodingDetection {
            encoding: XmlInputEncoding::Utf32Le,
            skip_bytes: 0,
        });
    }
    if matches!(bytes.get(..4), Some([0x00, b'<', 0x00, b'?'])) {
        return Ok(EncodingDetection {
            encoding: XmlInputEncoding::Utf16Be,
            skip_bytes: 0,
        });
    }
    if matches!(bytes.get(..4), Some([b'<', 0x00, b'?', 0x00])) {
        return Ok(EncodingDetection {
            encoding: XmlInputEncoding::Utf16Le,
            skip_bytes: 0,
        });
    }

    let encoding = match find_ascii_compatible_declared_encoding(bytes)? {
        Some(name) => encoding_from_label(name)?,
        None => XmlInputEncoding::Utf8,
    };

    Ok(EncodingDetection {
        encoding,
        skip_bytes: 0,
    })
}

#[cold]
#[inline(never)]
fn repeated_bom_error(offset: usize) -> XmlError {
    XmlError::new(XmlErrorKind::InvalidCharacter, offset)
}

fn decode_utf8(bytes: &[u8], skip: usize) -> XmlResult<Cow<'_, str>> {
    let bytes = &bytes[skip..];
    std::str::from_utf8(bytes)
        .map(Cow::Borrowed)
        .map_err(|error| XmlError::new(XmlErrorKind::InvalidCharacter, skip + error.valid_up_to()))
}

fn decode_ascii(bytes: &[u8], skip: usize) -> XmlResult<Cow<'_, str>> {
    let bytes = &bytes[skip..];
    if let Some(position) = bytes.iter().position(|byte| !byte.is_ascii()) {
        return Err(XmlError::new(
            XmlErrorKind::InvalidCharacter,
            skip + position,
        ));
    }
    decode_utf8(bytes, 0)
}

fn decode_latin1(bytes: &[u8], skip: usize) -> String {
    bytes[skip..].iter().map(|byte| char::from(*byte)).collect()
}

fn decode_utf16(bytes: &[u8], skip: usize, endian: Endian) -> XmlResult<String> {
    let bytes = &bytes[skip..];
    if bytes.len() % 2 != 0 {
        return Err(XmlError::new(
            XmlErrorKind::InvalidCharacter,
            skip + bytes.len() - 1,
        ));
    }

    let units = bytes.chunks_exact(2).map(|chunk| match endian {
        Endian::Little => u16::from_le_bytes([chunk[0], chunk[1]]),
        Endian::Big => u16::from_be_bytes([chunk[0], chunk[1]]),
    });

    let mut output = String::with_capacity(bytes.len() / 2);
    for (index, decoded) in char::decode_utf16(units).enumerate() {
        match decoded {
            Ok('\u{feff}') if index == 0 => {}
            Ok(ch) => output.push(ch),
            Err(_) => {
                return Err(XmlError::new(
                    XmlErrorKind::InvalidCharacter,
                    skip + index * 2,
                ));
            }
        }
    }
    Ok(output)
}

fn decode_utf32(bytes: &[u8], skip: usize, endian: Endian) -> XmlResult<String> {
    let bytes = &bytes[skip..];
    if bytes.len() % 4 != 0 {
        return Err(XmlError::new(
            XmlErrorKind::InvalidCharacter,
            skip + bytes.len() - (bytes.len() % 4),
        ));
    }

    let mut output = String::with_capacity(bytes.len() / 4);
    for (index, chunk) in bytes.chunks_exact(4).enumerate() {
        let value = match endian {
            Endian::Little => u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
            Endian::Big => u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
        };
        match char::from_u32(value) {
            Some('\u{feff}') if index == 0 => {}
            Some(ch) => output.push(ch),
            None => {
                return Err(XmlError::new(
                    XmlErrorKind::InvalidCharacter,
                    skip + index * 4,
                ));
            }
        }
    }
    Ok(output)
}

fn find_ascii_compatible_declared_encoding(bytes: &[u8]) -> XmlResult<Option<&str>> {
    let mut index = 0usize;
    while matches!(bytes.get(index), Some(byte) if is_space(*byte)) {
        index += 1;
    }
    if !bytes
        .get(index..)
        .is_some_and(|tail| tail.starts_with(b"<?xml"))
    {
        return Ok(None);
    }

    let Some(end) = find_ascii_pi_end(bytes, index + 5) else {
        return Ok(None);
    };
    let declaration = &bytes[index + 5..end];
    let mut cursor = 0usize;

    while cursor < declaration.len() {
        cursor = skip_ascii_space(declaration, cursor);
        if cursor >= declaration.len() {
            break;
        }
        let name_start = cursor;
        while matches!(
            declaration.get(cursor),
            Some(b'A'..=b'Z' | b'a'..=b'z' | b'_' | b':' | b'-')
        ) {
            cursor += 1;
        }
        if cursor == name_start {
            break;
        }
        let name = &declaration[name_start..cursor];
        cursor = skip_ascii_space(declaration, cursor);
        if declaration.get(cursor) != Some(&b'=') {
            break;
        }
        cursor += 1;
        cursor = skip_ascii_space(declaration, cursor);
        let Some(quote @ (b'\'' | b'"')) = declaration.get(cursor).copied() else {
            break;
        };
        cursor += 1;
        let value_start = cursor;
        while declaration.get(cursor).is_some_and(|byte| *byte != quote) {
            cursor += 1;
        }
        if declaration.get(cursor) != Some(&quote) {
            break;
        }
        if name.eq_ignore_ascii_case(b"encoding") {
            let value = std::str::from_utf8(&declaration[value_start..cursor])
                .map_err(|_| XmlError::new(XmlErrorKind::InvalidXmlDeclaration, index))?;
            return Ok(Some(value));
        }
        cursor += 1;
    }

    Ok(None)
}

#[inline(never)]
fn validate_declared_encoding(bytes: &[u8], detected: XmlInputEncoding) -> XmlResult<()> {
    let Some(label) = find_ascii_compatible_declared_encoding(bytes)? else {
        return Ok(());
    };
    if declared_encoding_matches(label, detected) {
        return Ok(());
    }
    Err(XmlError::new(XmlErrorKind::InvalidXmlDeclaration, 0))
}

fn declared_encoding_matches(label: &str, detected: XmlInputEncoding) -> bool {
    let normalized = label.trim().to_ascii_lowercase().replace('_', "-");
    match detected {
        XmlInputEncoding::Utf8 => matches!(normalized.as_str(), "utf-8" | "utf8"),
        XmlInputEncoding::UsAscii => matches!(normalized.as_str(), "us-ascii" | "ascii"),
        XmlInputEncoding::Utf16Le => {
            is_generic_utf16_label(&normalized) || normalized == "utf-16le"
        }
        XmlInputEncoding::Utf16Be => {
            is_generic_utf16_label(&normalized) || normalized == "utf-16be"
        }
        XmlInputEncoding::Utf32Le => {
            is_generic_utf32_label(&normalized)
                || matches!(normalized.as_str(), "utf-32le" | "ucs-4le")
        }
        XmlInputEncoding::Utf32Be => {
            is_generic_utf32_label(&normalized)
                || matches!(normalized.as_str(), "utf-32be" | "ucs-4be")
        }
        XmlInputEncoding::Latin1 => {
            matches!(normalized.as_str(), "iso-8859-1" | "latin1" | "latin-1")
        }
    }
}

fn is_generic_utf16_label(label: &str) -> bool {
    matches!(
        label,
        "utf-16" | "utf16" | "u16" | "ucs-2" | "iso-10646-ucs-2" | "csunicode"
    )
}

fn is_generic_utf32_label(label: &str) -> bool {
    matches!(
        label,
        "utf-32" | "utf32" | "ucs-4" | "iso-10646-ucs-4" | "csucs4"
    )
}

fn find_ascii_pi_end(bytes: &[u8], mut index: usize) -> Option<usize> {
    while index + 1 < bytes.len() {
        if bytes[index] == b'?' && bytes[index + 1] == b'>' {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn skip_ascii_space(bytes: &[u8], mut index: usize) -> usize {
    while matches!(bytes.get(index), Some(b' ' | b'\t' | b'\r' | b'\n')) {
        index += 1;
    }
    index
}

fn encoding_from_label(label: &str) -> XmlResult<XmlInputEncoding> {
    let normalized = label.trim().to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "utf-8" | "utf8" => Ok(XmlInputEncoding::Utf8),
        "us-ascii" | "ascii" => Ok(XmlInputEncoding::UsAscii),
        "utf-16" => Err(XmlError::new(
            XmlErrorKind::UnsupportedEncoding(label.to_owned()),
            0,
        )),
        "utf-16le" => Ok(XmlInputEncoding::Utf16Le),
        "utf-16be" => Ok(XmlInputEncoding::Utf16Be),
        "utf-32le" | "ucs-4le" => Ok(XmlInputEncoding::Utf32Le),
        "utf-32be" | "ucs-4be" => Ok(XmlInputEncoding::Utf32Be),
        "iso-8859-1" | "latin1" | "latin-1" => Ok(XmlInputEncoding::Latin1),
        _ => Err(XmlError::new(
            XmlErrorKind::UnsupportedEncoding(label.to_owned()),
            0,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{XmlDom, XmlTreeStats, count_document_bytes};
    use std::{fs, path::Path};

    #[test]
    fn decodes_utf8_without_copy() {
        let decoded = decode_xml_bytes(b"\xef\xbb\xbf<r/>").unwrap();

        assert_eq!(decoded.encoding, XmlInputEncoding::Utf8);
        assert_eq!(decoded.as_str(), "<r/>");
        assert!(matches!(decoded.input, Cow::Borrowed(_)));
    }

    #[test]
    fn decodes_latin1_from_declaration() {
        let decoded =
            decode_xml_bytes(b"<?xml version='1.0' encoding='ISO-8859-1'?><r>\xA3</r>").unwrap();

        assert_eq!(decoded.encoding, XmlInputEncoding::Latin1);
        assert_eq!(
            decoded.as_str(),
            "<?xml version='1.0' encoding='ISO-8859-1'?><r>£</r>"
        );
    }

    #[test]
    fn decodes_utf16_with_bom() {
        let bytes = [
            0xff, 0xfe, b'<', 0, b'r', 0, b'>', 0, 0xa3, 0, b'<', 0, b'/', 0, b'r', 0, b'>', 0,
        ];
        let decoded = decode_xml_bytes(&bytes).unwrap();

        assert_eq!(decoded.encoding, XmlInputEncoding::Utf16Le);
        assert_eq!(decoded.as_str(), "<r>£</r>");
    }

    #[test]
    fn decodes_utf32_by_signature() {
        let bytes = [b'<', 0, 0, 0, b'r', 0, 0, 0, b'/', 0, 0, 0, b'>', 0, 0, 0];
        let decoded = decode_xml_bytes(&bytes).unwrap();

        assert_eq!(decoded.encoding, XmlInputEncoding::Utf32Le);
        assert_eq!(decoded.as_str(), "<r/>");
    }

    #[test]
    fn accepts_matching_generic_unicode_declarations() {
        let utf16 = [
            &[0xff, 0xfe][..],
            &"<?xml version='1.0' encoding='UTF-16'?><r/>"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        ]
        .concat();
        let utf32 = [
            &[0x00, 0x00, 0xfe, 0xff][..],
            &"<?xml version='1.0' encoding='UTF-32'?><r/>"
                .chars()
                .flat_map(|ch| u32::from(ch).to_be_bytes())
                .collect::<Vec<_>>(),
        ]
        .concat();

        assert_eq!(
            decode_xml_bytes(&utf16).unwrap().encoding,
            XmlInputEncoding::Utf16Le
        );
        assert_eq!(
            decode_xml_bytes(&utf32).unwrap().encoding,
            XmlInputEncoding::Utf32Be
        );
    }

    #[test]
    fn rejects_repeated_boms_and_conflicting_declarations() {
        let repeated = b"\xef\xbb\xbf\xef\xbb\xbf<r/>";
        let conflict = b"\xef\xbb\xbf<?xml version='1.0' encoding='UTF-16LE'?><r/>";

        assert_eq!(
            decode_xml_bytes(repeated).unwrap_err(),
            XmlError::new(XmlErrorKind::InvalidCharacter, 3)
        );
        assert_eq!(
            decode_xml_bytes(conflict).unwrap_err().kind,
            XmlErrorKind::InvalidXmlDeclaration
        );
    }

    #[test]
    fn rejects_unsupported_ascii_compatible_encoding() {
        let error =
            decode_xml_bytes(b"<?xml version='1.0' encoding='Shift_JIS'?><r/>").unwrap_err();

        assert_eq!(
            error.kind,
            XmlErrorKind::UnsupportedEncoding("Shift_JIS".to_owned())
        );
    }

    #[test]
    fn parses_latin1_document_bytes() {
        let document =
            XmlDom::parse_bytes(b"<?xml version='1.0' encoding='ISO-8859-1'?><r>\xA3</r>").unwrap();

        assert_eq!(document.root().name().unwrap().as_deref(), Some("r"));
        assert_eq!(
            count_document_bytes(b"<r/>").unwrap(),
            XmlTreeStats {
                elements: 1,
                attributes: 0,
                nodes: 1,
            }
        );
    }

    #[test]
    fn parses_utf16_document_bytes() {
        let bytes = [0xfe, 0xff, 0x00, b'<', 0x00, b'r', 0x00, b'/', 0x00, b'>'];

        let document = XmlDom::parse_bytes(&bytes).unwrap();

        assert_eq!(document.root().name().unwrap().as_deref(), Some("r"));
    }

    #[test]
    fn vendored_encoding_fixtures_keep_expected_coverage() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/encoding");
        let mut files = Vec::new();
        collect_xml_files(&root, &mut files);

        let mut summary = FixtureSummary::default();
        for path in files {
            summary.files += 1;
            let bytes = fs::read(&path).unwrap_or_else(|error| {
                panic!("read {}: {error}", path.display());
            });
            match decode_xml_bytes(&bytes) {
                Ok(decoded) => {
                    summary.decoded += 1;
                    match decoded.encoding {
                        XmlInputEncoding::Utf8 => summary.utf8 += 1,
                        XmlInputEncoding::UsAscii => summary.ascii += 1,
                        XmlInputEncoding::Utf16Le | XmlInputEncoding::Utf16Be => {
                            summary.utf16 += 1;
                        }
                        XmlInputEncoding::Utf32Le | XmlInputEncoding::Utf32Be => {
                            summary.utf32 += 1;
                        }
                        XmlInputEncoding::Latin1 => summary.latin1 += 1,
                    }
                }
                Err(error) if matches!(error.kind, XmlErrorKind::UnsupportedEncoding(_)) => {
                    summary.unsupported += 1;
                }
                Err(_) => summary.decode_errors += 1,
            }
        }

        assert_eq!(
            summary,
            FixtureSummary {
                files: 296,
                decoded: 108,
                unsupported: 160,
                decode_errors: 28,
                utf8: 53,
                ascii: 0,
                utf16: 26,
                utf32: 10,
                latin1: 19,
            }
        );
    }

    #[derive(Debug, Default, Eq, PartialEq)]
    struct FixtureSummary {
        files: usize,
        decoded: usize,
        unsupported: usize,
        decode_errors: usize,
        utf8: usize,
        ascii: usize,
        utf16: usize,
        utf32: usize,
        latin1: usize,
    }

    fn collect_xml_files(path: &Path, output: &mut Vec<std::path::PathBuf>) {
        let metadata = fs::metadata(path).unwrap_or_else(|error| {
            panic!("metadata {}: {error}", path.display());
        });
        if metadata.is_file() {
            if path.extension().is_some_and(|extension| extension == "xml") {
                output.push(path.to_owned());
            }
            return;
        }

        let entries = fs::read_dir(path).unwrap_or_else(|error| {
            panic!("read dir {}: {error}", path.display());
        });
        for entry in entries {
            let entry = entry.unwrap_or_else(|error| {
                panic!("read dir entry {}: {error}", path.display());
            });
            collect_xml_files(&entry.path(), output);
        }
    }
}
