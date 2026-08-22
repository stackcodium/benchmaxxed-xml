use std::{fmt, io};

use crate::{
    RawXmlNode, XmlCompactDocument, XmlDoctype, XmlElement, XmlNode, XmlNodeKind, XmlPath,
    XmlProcessingInstruction, XmlViewNodeId,
    dom_facade::{
        OverlayNodeRef, SparseAttribute, SparseChild, SparseOverlay, SparseRelocation,
        overlay_has_subtree_edits, overlay_node_at,
    },
    dtd::validate_internal_subset,
    syntax::{is_pubid_char, is_xml11_char, is_xml11_literal_char},
};

pub(crate) fn compact_overlay_subtree_to_string_with_options(
    document: &XmlCompactDocument,
    edits: &SparseOverlay,
    path: &XmlPath,
    options: &XmlSerializeOptions,
    inner_xml: bool,
) -> Result<String, XmlWriteError> {
    validate_options(options)?;
    let mut output = String::new();
    let mut serializer = Serializer::to_string(&mut output, options);
    let result = serializer.compact_subtree(document, edits, path, inner_xml);
    serializer.finish()?;
    result?;
    Ok(output)
}

pub(crate) fn write_compact_overlay_subtree_with_options<W: io::Write>(
    document: &XmlCompactDocument,
    edits: &SparseOverlay,
    path: &XmlPath,
    mut writer: W,
    options: &XmlSerializeOptions,
    inner_xml: bool,
) -> Result<(), XmlWriteError> {
    validate_options(options)?;
    let mut serializer = Serializer::to_writer(&mut writer, options);
    let result = serializer.compact_subtree(document, edits, path, inner_xml);
    serializer.finish()?;
    result
}

pub(crate) fn write_compact_overlay<W: io::Write>(
    document: &XmlCompactDocument,
    edits: &SparseOverlay,
    mut writer: W,
) -> Result<(), XmlWriteError> {
    if document.default_serialization_is_source.get() == Some(true) && overlay_is_empty(edits) {
        writer.write_all(document.input.as_bytes())?;
        return Ok(());
    }
    if let Some(result) = write_sparse_source_overlay(document, edits, &mut writer) {
        return result;
    }
    let options = XmlSerializeOptions::default();
    let mut serializer = Serializer::to_writer(&mut writer, &options);
    let result = serializer.compact_document(document, edits);
    serializer.finish()?;
    result
}

fn overlay_is_empty(edits: &SparseOverlay) -> bool {
    edits.appended.is_empty()
        && edits.child_orders.is_empty()
        && edits.attributes.is_empty()
        && edits.added_attribute_order.is_empty()
        && edits.attribute_orders.is_empty()
        && edits.names.is_empty()
        && edits.values.is_empty()
        && edits.declaration.is_none()
        && edits.doctype.is_none()
        && edits.doctype_before_misc_index.is_none()
        && edits.misc_before_root.is_none()
        && edits.misc_after_root.is_none()
        && edits.relocations.is_empty()
}

pub(crate) fn write_compact_overlay_with_options<W: io::Write>(
    document: &XmlCompactDocument,
    edits: &SparseOverlay,
    mut writer: W,
    options: &XmlSerializeOptions,
) -> Result<(), XmlWriteError> {
    if options == &XmlSerializeOptions::default() {
        return write_compact_overlay(document, edits, writer);
    }
    validate_options(options)?;
    let mut serializer = Serializer::to_writer(&mut writer, options);
    let result = serializer.compact_document(document, edits);
    serializer.finish()?;
    result
}

pub(crate) fn compact_overlay_to_string_with_options(
    document: &XmlCompactDocument,
    edits: &SparseOverlay,
    options: &XmlSerializeOptions,
) -> Result<String, XmlWriteError> {
    validate_options(options)?;
    let learns_source_equivalence = options == &XmlSerializeOptions::default()
        && overlay_is_empty(edits)
        && document.default_serialization_is_source.get().is_none();
    if options == &XmlSerializeOptions::default()
        && overlay_is_empty(edits)
        && document.default_serialization_is_source.get() == Some(true)
    {
        return Ok(document.input.clone());
    }
    let mut output = String::new();
    let mut serializer = Serializer::to_string(&mut output, options);
    let result = serializer.compact_document(document, edits);
    serializer.finish()?;
    result?;
    if learns_source_equivalence {
        document
            .default_serialization_is_source
            .set(output == document.input);
    }
    Ok(output)
}

enum PatchReplacement<'a> {
    Empty,
    Source(&'a str),
    Text(String),
}

struct SourcePatch<'a> {
    start: usize,
    end: usize,
    replacement: PatchReplacement<'a>,
}

fn write_sparse_source_overlay<W: io::Write>(
    document: &XmlCompactDocument,
    edits: &SparseOverlay,
    writer: &mut W,
) -> Option<Result<(), XmlWriteError>> {
    if document.input.starts_with('\u{feff}')
        || edits.appended.values().any(|nodes| !nodes.is_empty())
        || !edits.child_orders.is_empty()
        || !edits.attribute_orders.is_empty()
        || !edits.names.is_empty()
        || !edits.values.is_empty()
        || edits.declaration.is_some()
        || edits.doctype.is_some()
        || edits.doctype_before_misc_index.is_some()
        || edits.misc_before_root.is_some()
        || edits.misc_after_root.is_some()
        || edits.relocations.len() != 1
    {
        return None;
    }

    let source = document.input.as_str();
    let relocation = &edits.relocations[0];
    let parent = compact_node_at_path(document, &relocation.parent)?;
    let source_node = document.children(parent).nth(relocation.source_index)?;
    let source_record = document.node(source_node)?;
    if source_record.kind() != XmlNodeKind::Element {
        return None;
    }
    let source_start = source_record.name_start.checked_sub(1)? as usize;
    let (_, source_end) = element_bounds(source, source_start)?;
    let parent_record = document.node(parent)?;
    let parent_start = parent_record.name_start.checked_sub(1)? as usize;
    let child_count = document.children(parent).count();
    let destination = if relocation.destination_index < child_count {
        let destination_node = document
            .children(parent)
            .nth(relocation.destination_index)?;
        compact_node_source_start(document.node(destination_node)?)?
    } else if relocation.parent.is_root() {
        compact_element_closing_start(document, parent)?
    } else {
        element_bounds(source, parent_start)?.0
    };
    if source_start >= source_end
        || source_end > source.len()
        || destination > source.len()
        || (destination > source_start && destination < source_end)
    {
        return None;
    }

    let mut patches = Vec::with_capacity(edits.attributes.len() + 2);
    patches.push(SourcePatch {
        start: source_start,
        end: source_end,
        replacement: PatchReplacement::Empty,
    });
    patches.push(SourcePatch {
        start: destination,
        end: destination,
        replacement: PatchReplacement::Source(&source[source_start..source_end]),
    });

    for ((path, name), value) in &edits.attributes {
        let id = compact_node_at_path(document, path)?;
        let record = document.node(id)?;
        if record.kind() != XmlNodeKind::Element {
            return Some(Err(XmlWriteError::InvalidName(name.clone())));
        }
        if let Err(error) = validate_name(name) {
            return Some(Err(error));
        }
        if !value.chars().all(crate::syntax::is_xml_char) {
            return Some(Err(XmlWriteError::InvalidCharacter));
        }
        let replacement = escape_default_attribute(value);
        if let Some(attribute_index) = record
            .attribute_range()
            .find(|index| document.attribute_name(*index) == Some(name.as_str()))
        {
            let attribute = &document.attributes[attribute_index];
            let start = attribute.value_start as usize;
            let end = start + attribute.value_len as usize;
            if start >= source_start && end <= source_end {
                return None;
            }
            patches.push(SourcePatch {
                start,
                end,
                replacement: PatchReplacement::Text(replacement),
            });
        } else {
            let element_start = record.name_start.checked_sub(1)? as usize;
            let tag_end = scan_markup_end(source, element_start)?;
            let insertion = if source.as_bytes().get(tag_end.wrapping_sub(2)) == Some(&b'/') {
                tag_end - 2
            } else {
                tag_end - 1
            };
            if insertion >= source_start && insertion <= source_end {
                return None;
            }
            patches.push(SourcePatch {
                start: insertion,
                end: insertion,
                replacement: PatchReplacement::Text(format!(" {name}=\"{replacement}\"")),
            });
        }
    }

    patches.sort_unstable_by_key(|patch| (patch.start, patch.end));
    let mut cursor = 0;
    for patch in patches {
        if patch.start < cursor || patch.end < patch.start || patch.end > source.len() {
            return None;
        }
        if let Err(error) = writer.write_all(&source.as_bytes()[cursor..patch.start]) {
            return Some(Err(error.into()));
        }
        let result = match patch.replacement {
            PatchReplacement::Empty => Ok(()),
            PatchReplacement::Source(value) => writer.write_all(value.as_bytes()),
            PatchReplacement::Text(value) => writer.write_all(value.as_bytes()),
        };
        if let Err(error) = result {
            return Some(Err(error.into()));
        }
        cursor = patch.end;
    }
    Some(
        writer
            .write_all(&source.as_bytes()[cursor..])
            .map_err(Into::into),
    )
}

fn compact_node_source_start(record: &RawXmlNode) -> Option<usize> {
    let content = record.name_start as usize;
    match record.kind() {
        XmlNodeKind::Element => content.checked_sub(1),
        XmlNodeKind::Text => Some(content),
        XmlNodeKind::Comment => content.checked_sub(4),
        XmlNodeKind::Cdata => content.checked_sub(9),
        XmlNodeKind::ProcessingInstruction => content.checked_sub(2),
    }
}

#[inline(never)]
fn compact_element_closing_start(
    document: &XmlCompactDocument,
    element: XmlViewNodeId,
) -> Option<usize> {
    let record = document.node(element)?;
    let last = record.next_subtree().checked_sub(1)?;
    if last <= element.index() {
        return None;
    }
    let source = document.input.as_str();
    let last_record = document.nodes.get(last)?;
    let mut cursor = compact_node_source_end(source, last_record)?;
    let requested = document.node_name(element)?;
    loop {
        let markup = cursor + source.get(cursor..)?.find('<')?;
        if source[markup..].starts_with("<!--") {
            cursor = markup + source.get(markup + 4..)?.find("-->")? + 7;
        } else if source[markup..].starts_with("<![CDATA[") {
            cursor = markup + source.get(markup + 9..)?.find("]]>")? + 12;
        } else if source[markup..].starts_with("<?") {
            cursor = markup + source.get(markup + 2..)?.find("?>")? + 4;
        } else if source[markup..].starts_with("</") {
            let end = scan_markup_end(source, markup)?;
            let name = source.get(markup + 2..)?;
            let name_end =
                name.find(|character: char| character.is_ascii_whitespace() || character == '>')?;
            if &name[..name_end] == requested {
                return Some(markup);
            }
            cursor = end;
        } else {
            return None;
        }
    }
}

fn compact_node_source_end(source: &str, record: &RawXmlNode) -> Option<usize> {
    let content_end = (record.name_start as usize).checked_add(record.name_len as usize)?;
    match record.kind() {
        XmlNodeKind::Element => {
            let start = record.name_start.checked_sub(1)? as usize;
            element_bounds(source, start).map(|bounds| bounds.1)
        }
        XmlNodeKind::Text => Some(content_end),
        XmlNodeKind::Comment | XmlNodeKind::Cdata => content_end.checked_add(3),
        XmlNodeKind::ProcessingInstruction => {
            let start = record.name_start.checked_sub(2)? as usize;
            Some(start + source.get(start..)?.find("?>")? + 2)
        }
    }
}

fn compact_node_at_path(document: &XmlCompactDocument, path: &XmlPath) -> Option<XmlViewNodeId> {
    let mut node = document.root();
    for index in path.indexes() {
        node = document.children(node).nth(*index)?;
    }
    Some(node)
}

fn scan_markup_end(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut index = start;
    let mut quote = None;
    while index < bytes.len() {
        match (bytes[index], quote) {
            (value, Some(active)) if value == active => quote = None,
            (b'\'' | b'"', None) => quote = Some(bytes[index]),
            (b'>', None) => return Some(index + 1),
            _ => {}
        }
        index += 1;
    }
    None
}

fn element_bounds(source: &str, start: usize) -> Option<(usize, usize)> {
    let bytes = source.as_bytes();
    if bytes.get(start) != Some(&b'<') {
        return None;
    }
    let first_end = scan_markup_end(source, start)?;
    if bytes.get(first_end.checked_sub(2)?) == Some(&b'/') {
        return Some((first_end, first_end));
    }
    let mut depth = 1usize;
    let mut cursor = first_end;
    loop {
        let relative = source.get(cursor..)?.find('<')?;
        let markup = cursor + relative;
        if source[markup..].starts_with("<!--") {
            cursor = markup + source.get(markup + 4..)?.find("-->")? + 7;
        } else if source[markup..].starts_with("<![CDATA[") {
            cursor = markup + source.get(markup + 9..)?.find("]]>")? + 12;
        } else if source[markup..].starts_with("<?") {
            cursor = markup + source.get(markup + 2..)?.find("?>")? + 4;
        } else {
            let end = scan_markup_end(source, markup)?;
            if source[markup..].starts_with("</") {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some((markup, end));
                }
            } else if bytes.get(end.checked_sub(2)?) != Some(&b'/') {
                depth += 1;
            }
            cursor = end;
        }
    }
}

fn escape_default_attribute(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '"' => output.push_str("&quot;"),
            '\t' => output.push_str("&#x9;"),
            '\n' => output.push_str("&#xA;"),
            '\r' => output.push_str("&#xD;"),
            _ => output.push(character),
        }
    }
    output
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// The supported `XmlDeclarationMode` alternatives.
pub enum XmlDeclarationMode {
    #[default]
    /// Indicates `Preserve`.
    Preserve,
    /// Indicates `Always`.
    Always,
    /// Indicates `Never`.
    Never,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// The supported `XmlOutputEncoding` alternatives.
pub enum XmlOutputEncoding {
    #[default]
    /// Indicates `Utf8`.
    Utf8,
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

impl XmlOutputEncoding {
    fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16LE",
            Self::Utf16Be => "UTF-16BE",
            Self::Utf32Le => "UTF-32LE",
            Self::Utf32Be => "UTF-32BE",
            Self::Latin1 => "ISO-8859-1",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// The supported `XmlQuoteStyle` alternatives.
pub enum XmlQuoteStyle {
    #[default]
    /// Indicates `Double`.
    Double,
    /// Indicates `Single`.
    Single,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// The supported `XmlEscapeMode` alternatives.
pub enum XmlEscapeMode {
    #[default]
    /// Indicates `Escaped`.
    Escaped,
    /// Writes text/attributes unchanged only when they contain no markup delimiters.
    RawChecked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A public `XmlSerializeOptions` value in the XML data model.
pub struct XmlSerializeOptions {
    /// `None` produces compact output. `Some` contains one indentation unit.
    pub indent: Option<String>,
    /// The line ending.
    pub line_ending: String,
    /// The declaration.
    pub declaration: XmlDeclarationMode,
    /// The encoding.
    pub encoding: XmlOutputEncoding,
    /// XML language version to declare and enforce while serializing.
    pub version: crate::XmlVersion,
    /// Writes the selected encoding's BOM before the serialized document or subtree.
    pub write_bom: bool,
    /// The quote style.
    pub quote_style: XmlQuoteStyle,
    /// The indent attributes.
    pub indent_attributes: bool,
    /// The escape mode.
    pub escape_mode: XmlEscapeMode,
    /// The expand empty elements.
    pub expand_empty_elements: bool,
    /// The max depth.
    pub max_depth: usize,
}

impl Default for XmlSerializeOptions {
    fn default() -> Self {
        Self {
            indent: None,
            line_ending: "\n".to_owned(),
            declaration: XmlDeclarationMode::Preserve,
            encoding: XmlOutputEncoding::Utf8,
            version: crate::XmlVersion::Xml10,
            write_bom: false,
            quote_style: XmlQuoteStyle::Double,
            indent_attributes: false,
            escape_mode: XmlEscapeMode::Escaped,
            expand_empty_elements: false,
            max_depth: 1_024,
        }
    }
}

impl XmlSerializeOptions {
    /// Returns pretty.
    pub fn pretty() -> Self {
        Self {
            indent: Some("\t".to_owned()),
            ..Self::default()
        }
    }
}

#[derive(Debug)]
/// The supported `XmlWriteError` alternatives.
pub enum XmlWriteError {
    /// Indicates `InvalidName`.
    InvalidName(String),
    /// Indicates `InvalidCharacter`.
    InvalidCharacter,
    /// Indicates `InvalidComment`.
    InvalidComment,
    /// Indicates `InvalidProcessingInstruction`.
    InvalidProcessingInstruction,
    /// Indicates `InvalidDoctype`.
    InvalidDoctype,
    /// Indicates `UnrepresentableCharacter`.
    UnrepresentableCharacter {
        /// The character.
        character: char,
        /// The encoding.
        encoding: XmlOutputEncoding,
    },
    /// Indicates `InvalidFormatting`.
    InvalidFormatting,
    /// Indicates `DepthLimitExceeded`.
    DepthLimitExceeded,
    /// Indicates `Io`.
    Io(io::Error),
}

impl fmt::Display for XmlWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(name) => write!(formatter, "invalid XML name {name:?}"),
            Self::InvalidCharacter => formatter.write_str("invalid XML character"),
            Self::InvalidComment => formatter.write_str("invalid XML comment"),
            Self::InvalidProcessingInstruction => {
                formatter.write_str("invalid XML processing instruction")
            }
            Self::InvalidDoctype => formatter.write_str("invalid XML document type"),
            Self::UnrepresentableCharacter {
                character,
                encoding,
            } => write!(
                formatter,
                "character {character:?} is not representable in {encoding:?} output"
            ),
            Self::InvalidFormatting => {
                formatter.write_str("indent and line ending must contain only XML whitespace")
            }
            Self::DepthLimitExceeded => formatter.write_str("serialization depth limit exceeded"),
            Self::Io(error) => write!(formatter, "XML output I/O error: {error}"),
        }
    }
}

impl std::error::Error for XmlWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for XmlWriteError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl XmlElement {
    /// Returns to xml string.
    pub fn to_xml_string(&self) -> Result<String, XmlWriteError> {
        self.to_xml_string_with_options(&XmlSerializeOptions::default())
    }

    /// Returns to xml string with options.
    pub fn to_xml_string_with_options(
        &self,
        options: &XmlSerializeOptions,
    ) -> Result<String, XmlWriteError> {
        validate_options(options)?;
        let mut output = String::new();
        let mut serializer = Serializer::to_string(&mut output, options);
        let result = serializer.element_output(self);
        serializer.finish()?;
        result?;
        Ok(output)
    }

    /// Serializes this element and its descendants directly to a byte writer.
    pub fn write_xml<W: io::Write>(&self, writer: W) -> Result<(), XmlWriteError> {
        self.write_xml_with_options(writer, &XmlSerializeOptions::default())
    }

    /// Writes xml with options.
    pub fn write_xml_with_options<W: io::Write>(
        &self,
        mut writer: W,
        options: &XmlSerializeOptions,
    ) -> Result<(), XmlWriteError> {
        validate_options(options)?;
        let mut serializer = Serializer::to_writer(&mut writer, options);
        let result = serializer.element_output(self);
        serializer.finish()?;
        result
    }
}

impl XmlCompactDocument {
    /// Serializes the complete compact document with default options.
    pub fn to_xml_string(&self) -> Result<String, XmlWriteError> {
        self.to_xml_string_with_options(&XmlSerializeOptions::default())
    }

    /// Serializes the complete compact document with explicit options.
    pub fn to_xml_string_with_options(
        &self,
        options: &XmlSerializeOptions,
    ) -> Result<String, XmlWriteError> {
        compact_overlay_to_string_with_options(self, &SparseOverlay::default(), options)
    }

    /// Serializes the complete compact document to a byte writer with default options.
    pub fn write_xml<W: io::Write>(&self, writer: W) -> Result<(), XmlWriteError> {
        write_compact_overlay(self, &SparseOverlay::default(), writer)
    }

    /// Serializes the complete compact document to a byte writer with explicit options.
    pub fn write_xml_with_options<W: io::Write>(
        &self,
        writer: W,
        options: &XmlSerializeOptions,
    ) -> Result<(), XmlWriteError> {
        write_compact_overlay_with_options(self, &SparseOverlay::default(), writer, options)
    }
}

struct Serializer<'options, O> {
    output: O,
    options: &'options XmlSerializeOptions,
    structural_source_ranges: bool,
}

const MATERIALIZED_RECURSION_LIMIT: usize = 1_024;

struct MaterializedElementFrame<'a> {
    element: &'a XmlElement,
    depth: usize,
    next_child: usize,
    block_content: bool,
}

trait SerializerOutput {
    fn push_str(&mut self, value: &str);

    fn push(&mut self, character: char) {
        let mut bytes = [0; 4];
        self.push_str(character.encode_utf8(&mut bytes));
    }

    fn finish(self) -> Result<(), XmlWriteError>;
}

struct StringOutput<'a>(&'a mut String);

impl SerializerOutput for StringOutput<'_> {
    fn push_str(&mut self, value: &str) {
        self.0.push_str(value);
    }

    fn finish(self) -> Result<(), XmlWriteError> {
        Ok(())
    }
}

struct WriterOutput<'a, W> {
    writer: &'a mut W,
    encoding: XmlOutputEncoding,
    error: Option<XmlWriteError>,
}

impl<W: io::Write> SerializerOutput for WriterOutput<'_, W> {
    fn push_str(&mut self, value: &str) {
        if self.error.is_none() {
            if let Err(source) = write_encoded(self.writer, value, self.encoding) {
                self.error = Some(source);
            }
        }
    }

    fn finish(self) -> Result<(), XmlWriteError> {
        self.error.map_or(Ok(()), Err)
    }
}

fn write_encoded<W: io::Write + ?Sized>(
    writer: &mut W,
    value: &str,
    encoding: XmlOutputEncoding,
) -> Result<(), XmlWriteError> {
    if encoding == XmlOutputEncoding::Utf8 {
        writer.write_all(value.as_bytes())?;
        return Ok(());
    }
    for character in value.chars() {
        match encoding {
            XmlOutputEncoding::Utf8 => unreachable!(),
            XmlOutputEncoding::Latin1 => {
                let code = u32::from(character);
                if code > 0xff {
                    return Err(XmlWriteError::UnrepresentableCharacter {
                        character,
                        encoding,
                    });
                }
                writer.write_all(&[code as u8])?;
            }
            XmlOutputEncoding::Utf16Le | XmlOutputEncoding::Utf16Be => {
                let mut units = [0u16; 2];
                for unit in character.encode_utf16(&mut units).iter().copied() {
                    let bytes = if encoding == XmlOutputEncoding::Utf16Le {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    };
                    writer.write_all(&bytes)?;
                }
            }
            XmlOutputEncoding::Utf32Le | XmlOutputEncoding::Utf32Be => {
                let code = u32::from(character);
                let bytes = if encoding == XmlOutputEncoding::Utf32Le {
                    code.to_le_bytes()
                } else {
                    code.to_be_bytes()
                };
                writer.write_all(&bytes)?;
            }
        }
    }
    Ok(())
}

impl<'a, 'options> Serializer<'options, StringOutput<'a>> {
    fn to_string(output: &'a mut String, options: &'options XmlSerializeOptions) -> Self {
        Self {
            output: StringOutput(output),
            options,
            structural_source_ranges: false,
        }
    }
}

impl<'a, 'options, W: io::Write> Serializer<'options, WriterOutput<'a, W>> {
    fn to_writer(writer: &'a mut W, options: &'options XmlSerializeOptions) -> Self {
        Self {
            output: WriterOutput {
                writer,
                encoding: options.encoding,
                error: None,
            },
            options,
            structural_source_ranges: false,
        }
    }
}

impl<O: SerializerOutput> Serializer<'_, O> {
    fn compact_subtree(
        &mut self,
        document: &XmlCompactDocument,
        edits: &SparseOverlay,
        path: &XmlPath,
        inner_xml: bool,
    ) -> Result<(), XmlWriteError> {
        match overlay_node_at(document, edits, path).expect("validated facade path") {
            OverlayNodeRef::Compact(id) => {
                let mut path = path.clone();
                if inner_xml {
                    self.compact_inner_xml(document, edits, id, &mut path)
                } else {
                    self.compact_child(document, edits, id, &mut path, 0)
                }
            }
            OverlayNodeRef::Materialized(node) => {
                if inner_xml {
                    for child in &node
                        .as_element()
                        .expect("inner XML target was validated as an element")
                        .children
                    {
                        self.node(child, 0)?;
                    }
                    Ok(())
                } else {
                    self.node(node, 0)
                }
            }
        }
    }

    fn compact_inner_xml(
        &mut self,
        document: &XmlCompactDocument,
        edits: &SparseOverlay,
        id: XmlViewNodeId,
        path: &mut XmlPath,
    ) -> Result<(), XmlWriteError> {
        if let Some(children) = edits.child_orders.get(path) {
            for (index, child) in children.iter().enumerate() {
                match child {
                    SparseChild::Compact(id) | SparseChild::CompactCopy { id, .. } => {
                        path.indexes_mut().push(index);
                        self.compact_child(document, edits, *id, path, 0)?;
                        path.indexes_mut().pop();
                    }
                    SparseChild::Materialized(node) => self.node(node, 0)?,
                }
            }
            return Ok(());
        }

        let mut children: Vec<CompactOverlayChild<'_>> = document
            .children(id)
            .map(|id| CompactOverlayChild::Compact { id })
            .collect();
        children.extend(
            edits
                .appended
                .get(path)
                .into_iter()
                .flatten()
                .map(CompactOverlayChild::Materialized),
        );
        if let Some(relocation) = edits.relocations.iter().find(|entry| entry.parent == *path) {
            let moved = children.remove(relocation.source_index);
            let destination = relocation.destination_index
                - usize::from(relocation.source_index < relocation.destination_index);
            children.insert(destination, moved);
        }
        for (index, child) in children.into_iter().enumerate() {
            match child {
                CompactOverlayChild::Compact { id } => {
                    path.indexes_mut().push(index);
                    self.compact_child(document, edits, id, path, 0)?;
                    path.indexes_mut().pop();
                }
                CompactOverlayChild::Materialized(node) => self.node(node, 0)?,
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<(), XmlWriteError> {
        self.output.finish()
    }

    fn compact_document(
        &mut self,
        document: &XmlCompactDocument,
        edits: &SparseOverlay,
    ) -> Result<(), XmlWriteError> {
        self.structural_source_ranges = self.options == &XmlSerializeOptions::default()
            && !edits.child_orders.is_empty()
            && !document.input.contains("<!ENTITY");
        self.bom();
        let preserved_declaration = edits
            .declaration
            .as_ref()
            .map_or(document.metadata.declaration.as_ref(), Option::as_ref);
        match self.options.declaration {
            XmlDeclarationMode::Preserve => {
                if let Some(declaration) = preserved_declaration {
                    self.declaration(declaration)?;
                    self.document_separator();
                }
            }
            XmlDeclarationMode::Always => {
                let declaration =
                    preserved_declaration
                        .cloned()
                        .unwrap_or_else(|| XmlProcessingInstruction {
                            target: "xml".to_owned(),
                            data: format!(
                                "version=\"{}\" encoding=\"{}\"",
                                self.output_version(),
                                self.options.encoding.label()
                            ),
                        });
                self.declaration(&declaration)?;
                self.document_separator();
            }
            XmlDeclarationMode::Never => {}
        }

        let before = edits
            .misc_before_root
            .as_deref()
            .unwrap_or(&document.metadata.misc_before_root);
        let after = edits
            .misc_after_root
            .as_deref()
            .unwrap_or(&document.metadata.misc_after_root);
        let doctype = edits
            .doctype
            .as_ref()
            .map_or(document.metadata.doctype.as_ref(), Option::as_ref);
        let doctype_index = edits
            .doctype_before_misc_index
            .unwrap_or(document.metadata.doctype_before_misc_index)
            .unwrap_or(before.len())
            .min(before.len());
        for (index, node) in before.iter().enumerate() {
            if index == doctype_index {
                if let Some(doctype) = doctype {
                    self.doctype(doctype)?;
                    self.document_separator();
                }
            }
            self.node(node, 0)?;
            self.document_separator();
        }
        if doctype_index == before.len() {
            if let Some(doctype) = doctype {
                self.doctype(doctype)?;
                self.document_separator();
            }
        }

        let mut path = XmlPath::root();
        self.compact_element(document, edits, document.root(), &mut path, 0)?;
        for node in after {
            self.document_separator();
            self.node(node, 0)?;
        }
        Ok(())
    }

    fn compact_element(
        &mut self,
        document: &XmlCompactDocument,
        edits: &SparseOverlay,
        id: XmlViewNodeId,
        path: &mut XmlPath,
        depth: usize,
    ) -> Result<(), XmlWriteError> {
        if depth > self.options.max_depth {
            return Err(XmlWriteError::DepthLimitExceeded);
        }
        let record = document.node(id).expect("compact element record");
        let name = edits
            .names
            .get(path)
            .map(String::as_str)
            .unwrap_or_else(|| document.node_name(id).expect("compact element name"));
        if edits.names.contains_key(path) {
            validate_name(name)?;
        }
        self.output.push('<');
        self.output.push_str(name);
        let quote = match self.options.quote_style {
            XmlQuoteStyle::Double => '"',
            XmlQuoteStyle::Single => '\'',
        };

        if let Some(attributes) = edits.attribute_orders.get(path) {
            for attribute in attributes {
                match attribute {
                    SparseAttribute::Compact(attribute_index) => {
                        let name = document
                            .attribute_name(*attribute_index)
                            .expect("compact attribute name");
                        self.output.push(' ');
                        self.output.push_str(name);
                        self.output.push('=');
                        self.output.push(quote);
                        if let Some(value) = overlay_attribute(edits, path, name) {
                            self.attribute_value(value, quote)?;
                        } else {
                            self.raw_attribute_value(
                                document
                                    .attribute_value(*attribute_index)
                                    .expect("compact attribute value"),
                                quote,
                            );
                        }
                        self.output.push(quote);
                    }
                    SparseAttribute::Materialized(attribute) => {
                        validate_name(&attribute.name)?;
                        self.output.push(' ');
                        self.output.push_str(&attribute.name);
                        self.output.push('=');
                        self.output.push(quote);
                        self.attribute_value(&attribute.value, quote)?;
                        self.output.push(quote);
                    }
                }
            }
        } else {
            for attribute_index in record.attribute_range() {
                let attribute_name = document
                    .attribute_name(attribute_index)
                    .expect("compact attribute name");
                self.output.push(' ');
                self.output.push_str(attribute_name);
                self.output.push('=');
                self.output.push(quote);
                if let Some(value) = overlay_attribute(edits, path, attribute_name) {
                    self.attribute_value(value, quote)?;
                } else {
                    self.raw_attribute_value(
                        document
                            .attribute_value(attribute_index)
                            .expect("compact attribute value"),
                        quote,
                    );
                }
                self.output.push(quote);
            }
            if let Some(added_order) = edits.added_attribute_order.get(path) {
                for name in added_order {
                    if record.attribute_range().any(|attribute_index| {
                        document.attribute_name(attribute_index) == Some(name.as_str())
                    }) {
                        continue;
                    }
                    let Some(value) = edits.attributes.get(&(path.clone(), name.clone())) else {
                        continue;
                    };
                    self.output.push(' ');
                    self.output.push_str(name);
                    self.output.push('=');
                    self.output.push(quote);
                    self.attribute_value(value, quote)?;
                    self.output.push(quote);
                }
            }
            let mut added_attributes: Vec<_> = edits
                .attributes
                .iter()
                .filter(|((target, name), _)| {
                    target == path
                        && !edits
                            .added_attribute_order
                            .get(path)
                            .is_some_and(|order| order.contains(name))
                        && !record.attribute_range().any(|attribute_index| {
                            document.attribute_name(attribute_index) == Some(name.as_str())
                        })
                })
                .collect();
            added_attributes.sort_unstable_by(|left, right| left.0.1.cmp(&right.0.1));
            for ((_, name), value) in added_attributes {
                self.output.push(' ');
                self.output.push_str(name);
                self.output.push('=');
                self.output.push(quote);
                self.attribute_value(value, quote)?;
                self.output.push(quote);
            }
        }

        let appended = edits.appended.get(path).map(Vec::as_slice).unwrap_or(&[]);
        let has_children = edits.child_orders.get(path).map_or_else(
            || record.first_child().is_some() || !appended.is_empty(),
            |v| !v.is_empty(),
        );
        if !has_children {
            self.output.push_str("/>");
            return Ok(());
        }
        self.output.push('>');

        if let Some(children) = edits.child_orders.get(path) {
            for (index, child) in children.iter().enumerate() {
                match child {
                    SparseChild::Compact(id) | SparseChild::CompactCopy { id, .. } => {
                        path.indexes_mut().push(index);
                        self.compact_child(document, edits, *id, path, depth + 1)?;
                        path.indexes_mut().pop();
                    }
                    SparseChild::Materialized(node) => self.node(node, depth + 1)?,
                }
            }
        } else if let Some(relocation) =
            edits.relocations.iter().find(|entry| entry.parent == *path)
        {
            self.compact_relocated_children(document, edits, id, path, depth, relocation)?;
        } else {
            for (index, child) in document.children(id).enumerate() {
                path.indexes_mut().push(index);
                self.compact_child(document, edits, child, path, depth + 1)?;
                path.indexes_mut().pop();
            }
            for child in appended {
                self.node(child, depth + 1)?;
            }
        }
        self.output.push_str("</");
        self.output.push_str(name);
        self.output.push('>');
        Ok(())
    }

    fn compact_relocated_children(
        &mut self,
        document: &XmlCompactDocument,
        edits: &SparseOverlay,
        id: XmlViewNodeId,
        path: &mut XmlPath,
        depth: usize,
        relocation: &SparseRelocation,
    ) -> Result<(), XmlWriteError> {
        let mut children: Vec<CompactOverlayChild<'_>> = document
            .children(id)
            .map(|id| CompactOverlayChild::Compact { id })
            .collect();
        children.extend(
            edits
                .appended
                .get(path)
                .into_iter()
                .flatten()
                .map(CompactOverlayChild::Materialized),
        );
        let moved = children.remove(relocation.source_index);
        let destination = relocation.destination_index
            - usize::from(relocation.source_index < relocation.destination_index);
        children.insert(destination, moved);

        for (logical_index, child) in children.into_iter().enumerate() {
            match child {
                CompactOverlayChild::Compact { id } => {
                    path.indexes_mut().push(logical_index);
                    self.compact_child(document, edits, id, path, depth + 1)?;
                    path.indexes_mut().pop();
                }
                CompactOverlayChild::Materialized(node) => self.node(node, depth + 1)?,
            }
        }
        Ok(())
    }

    fn compact_child(
        &mut self,
        document: &XmlCompactDocument,
        edits: &SparseOverlay,
        id: XmlViewNodeId,
        path: &mut XmlPath,
        depth: usize,
    ) -> Result<(), XmlWriteError> {
        let record = document.node(id).expect("compact child record");
        if record.kind() == XmlNodeKind::Element
            && !path.is_root()
            && self.structural_source_ranges
            && !overlay_has_subtree_edits(edits, path)
        {
            let start = compact_node_source_start(record).expect("element source starts with '<'");
            let (_, end) = element_bounds(&document.input, start)
                .expect("validated compact element has complete source bounds");
            self.output.push_str(&document.input[start..end]);
            return Ok(());
        }
        match record.kind() {
            XmlNodeKind::Element => self.compact_element(document, edits, id, path, depth),
            kind @ (XmlNodeKind::Text | XmlNodeKind::Cdata) => {
                if let Some(value) = edits.values.get(path) {
                    return if kind == XmlNodeKind::Text {
                        self.text(value)
                    } else {
                        self.cdata(value)
                    };
                }
                if kind == XmlNodeKind::Text {
                    self.output
                        .push_str(document.node_value(id).expect("compact text"));
                } else {
                    self.output.push_str("<![CDATA[");
                    self.output
                        .push_str(document.node_value(id).expect("compact CDATA"));
                    self.output.push_str("]]>");
                }
                Ok(())
            }
            XmlNodeKind::Comment => {
                if let Some(value) = edits.values.get(path) {
                    return self.comment(value);
                }
                self.output.push_str("<!--");
                self.output.push_str(compact_primary(document, record));
                self.output.push_str("-->");
                Ok(())
            }
            XmlNodeKind::ProcessingInstruction => {
                if edits.names.contains_key(path) || edits.values.contains_key(path) {
                    let target = edits
                        .names
                        .get(path)
                        .map(String::as_str)
                        .unwrap_or_else(|| document.node_name(id).expect("compact PI target"));
                    let data = edits
                        .values
                        .get(path)
                        .map(String::as_str)
                        .unwrap_or_else(|| compact_secondary(document, record));
                    return self.processing_instruction(&XmlProcessingInstruction {
                        target: target.to_owned(),
                        data: data.to_owned(),
                    });
                }
                self.output.push_str("<?");
                self.output
                    .push_str(document.node_name(id).expect("compact PI target"));
                let data = compact_secondary(document, record);
                if !data.is_empty() {
                    self.output.push(' ');
                    self.output.push_str(data);
                }
                self.output.push_str("?>");
                Ok(())
            }
        }
    }

    fn raw_attribute_value(&mut self, value: &str, quote: char) {
        let mut remaining = value;
        while let Some(index) = remaining.find(quote) {
            self.output.push_str(&remaining[..index]);
            self.output
                .push_str(if quote == '"' { "&quot;" } else { "&apos;" });
            remaining = &remaining[index + quote.len_utf8()..];
        }
        self.output.push_str(remaining);
    }

    fn element_output(&mut self, element: &XmlElement) -> Result<(), XmlWriteError> {
        self.bom();
        self.element(element, 0)
    }

    fn bom(&mut self) {
        if self.options.write_bom {
            self.output.push('\u{feff}');
        }
    }

    fn declaration(&mut self, declaration: &XmlProcessingInstruction) -> Result<(), XmlWriteError> {
        let mut data = declaration.data.clone();
        if self.options.encoding != XmlOutputEncoding::Utf8
            || self.options.version != crate::XmlVersion::Xml10
        {
            data = set_declaration_value(&data, "version", self.output_version());
            data = set_declaration_value(&data, "encoding", self.options.encoding.label());
        }
        if data.contains("?>") {
            return Err(XmlWriteError::InvalidProcessingInstruction);
        }
        self.output.push_str("<?xml");
        if !data.is_empty() {
            self.output.push(' ');
            self.output.push_str(&data);
        }
        self.output.push_str("?>");
        Ok(())
    }

    fn output_version(&self) -> &'static str {
        match self.options.version {
            crate::XmlVersion::Xml10 => "1.0",
            crate::XmlVersion::Xml11 => "1.1",
        }
    }

    fn doctype(&mut self, doctype: &XmlDoctype) -> Result<(), XmlWriteError> {
        validate_name(&doctype.name)?;
        if doctype.public_id.is_some() && doctype.system_id.is_none() {
            return Err(XmlWriteError::InvalidDoctype);
        }
        self.output.push_str("<!DOCTYPE ");
        self.output.push_str(&doctype.name);
        if let Some(public_id) = &doctype.public_id {
            if !public_id.chars().all(is_pubid_char) {
                return Err(XmlWriteError::InvalidDoctype);
            }
            self.output.push_str(" PUBLIC ");
            self.quoted_doctype_literal(public_id)?;
            self.output.push(' ');
            self.quoted_doctype_literal(
                doctype
                    .system_id
                    .as_deref()
                    .expect("public identifier requires system identifier"),
            )?;
        } else if let Some(system_id) = &doctype.system_id {
            self.output.push_str(" SYSTEM ");
            self.quoted_doctype_literal(system_id)?;
        }
        if let Some(subset) = &doctype.internal_subset {
            validate_internal_subset(subset, 0, self.options.version == crate::XmlVersion::Xml11)
                .map_err(|_| XmlWriteError::InvalidDoctype)?;
            self.output.push_str(" [");
            self.output.push_str(subset);
            self.output.push(']');
        }
        self.output.push('>');
        Ok(())
    }

    fn quoted_doctype_literal(&mut self, value: &str) -> Result<(), XmlWriteError> {
        validate_xml_chars(value)?;
        let quote = if !value.contains('"') {
            '"'
        } else if !value.contains('\'') {
            '\''
        } else {
            return Err(XmlWriteError::InvalidDoctype);
        };
        self.output.push(quote);
        self.output.push_str(value);
        self.output.push(quote);
        Ok(())
    }

    fn document_separator(&mut self) {
        if self.options.indent.is_some() {
            self.output.push_str(&self.options.line_ending);
        }
    }

    fn node(&mut self, node: &XmlNode, depth: usize) -> Result<(), XmlWriteError> {
        if self.options.max_depth <= MATERIALIZED_RECURSION_LIMIT {
            self.node_recursive::<false>(node, depth)
        } else {
            self.node_with_raised_limit(node, depth)
        }
    }

    #[cold]
    #[inline(never)]
    fn node_with_raised_limit(
        &mut self,
        node: &XmlNode,
        depth: usize,
    ) -> Result<(), XmlWriteError> {
        self.node_recursive::<true>(node, depth)
    }

    fn node_recursive<const SWITCH_TO_ITERATIVE: bool>(
        &mut self,
        node: &XmlNode,
        depth: usize,
    ) -> Result<(), XmlWriteError> {
        match node {
            XmlNode::Element(element) => {
                self.element_recursive::<SWITCH_TO_ITERATIVE>(element, depth)
            }
            XmlNode::Text(value) => self.text(value),
            XmlNode::Comment(value) => self.comment(value),
            XmlNode::Cdata(value) => self.cdata(value),
            XmlNode::ProcessingInstruction(pi) => self.processing_instruction(pi),
        }
    }

    fn element(&mut self, element: &XmlElement, depth: usize) -> Result<(), XmlWriteError> {
        if self.options.max_depth <= MATERIALIZED_RECURSION_LIMIT {
            self.element_recursive::<false>(element, depth)
        } else {
            self.element_with_raised_limit(element, depth)
        }
    }

    #[cold]
    #[inline(never)]
    fn element_with_raised_limit(
        &mut self,
        element: &XmlElement,
        depth: usize,
    ) -> Result<(), XmlWriteError> {
        self.element_recursive::<true>(element, depth)
    }

    fn element_recursive<const SWITCH_TO_ITERATIVE: bool>(
        &mut self,
        element: &XmlElement,
        depth: usize,
    ) -> Result<(), XmlWriteError> {
        if depth > self.options.max_depth {
            return Err(XmlWriteError::DepthLimitExceeded);
        }
        if SWITCH_TO_ITERATIVE && depth >= MATERIALIZED_RECURSION_LIMIT {
            return self.element_iterative(element, depth);
        }
        validate_name(&element.name)?;
        self.output.push('<');
        self.output.push_str(&element.name);
        for attribute in &element.attributes {
            validate_name(&attribute.name)?;
            if self.options.indent_attributes && self.options.indent.is_some() {
                self.output.push_str(&self.options.line_ending);
                self.indent(depth + 1);
            } else {
                self.output.push(' ');
            }
            self.output.push_str(&attribute.name);
            let quote = match self.options.quote_style {
                XmlQuoteStyle::Double => '"',
                XmlQuoteStyle::Single => '\'',
            };
            self.output.push('=');
            self.output.push(quote);
            self.attribute_value(&attribute.value, quote)?;
            self.output.push(quote);
        }

        if element.children.is_empty() && !self.options.expand_empty_elements {
            self.output.push_str("/>");
            return Ok(());
        }

        self.output.push('>');
        let block_content = self.options.indent.is_some()
            && element
                .children
                .iter()
                .all(|child| !matches!(child, XmlNode::Text(_) | XmlNode::Cdata(_)));
        for child in &element.children {
            if block_content {
                self.output.push_str(&self.options.line_ending);
                self.indent(depth + 1);
            }
            self.node_recursive::<SWITCH_TO_ITERATIVE>(child, depth + 1)?;
        }
        if block_content && !element.children.is_empty() {
            self.output.push_str(&self.options.line_ending);
            self.indent(depth);
        }
        self.output.push_str("</");
        self.output.push_str(&element.name);
        self.output.push('>');
        Ok(())
    }

    #[cold]
    #[inline(never)]
    fn element_iterative(
        &mut self,
        element: &XmlElement,
        depth: usize,
    ) -> Result<(), XmlWriteError> {
        let Some(frame) = self.begin_iterative_element(element, depth)? else {
            return Ok(());
        };
        let mut stack = Vec::with_capacity(64);
        stack.push(frame);
        while let Some(frame) = stack.last_mut() {
            if frame.next_child == frame.element.children.len() {
                if frame.block_content && !frame.element.children.is_empty() {
                    self.output.push_str(&self.options.line_ending);
                    self.indent(frame.depth);
                }
                self.output.push_str("</");
                self.output.push_str(&frame.element.name);
                self.output.push('>');
                stack.pop();
                continue;
            }

            let child = &frame.element.children[frame.next_child];
            frame.next_child += 1;
            let child_depth = frame.depth + 1;
            if frame.block_content {
                self.output.push_str(&self.options.line_ending);
                self.indent(child_depth);
            }
            match child {
                XmlNode::Element(element) => {
                    if let Some(frame) = self.begin_iterative_element(element, child_depth)? {
                        stack.push(frame);
                    }
                }
                XmlNode::Text(value) => self.text(value)?,
                XmlNode::Comment(value) => self.comment(value)?,
                XmlNode::Cdata(value) => self.cdata(value)?,
                XmlNode::ProcessingInstruction(pi) => self.processing_instruction(pi)?,
            }
        }
        Ok(())
    }

    fn begin_iterative_element<'a>(
        &mut self,
        element: &'a XmlElement,
        depth: usize,
    ) -> Result<Option<MaterializedElementFrame<'a>>, XmlWriteError> {
        if depth > self.options.max_depth {
            return Err(XmlWriteError::DepthLimitExceeded);
        }
        validate_name(&element.name)?;
        self.output.push('<');
        self.output.push_str(&element.name);
        for attribute in &element.attributes {
            validate_name(&attribute.name)?;
            if self.options.indent_attributes && self.options.indent.is_some() {
                self.output.push_str(&self.options.line_ending);
                self.indent(depth + 1);
            } else {
                self.output.push(' ');
            }
            self.output.push_str(&attribute.name);
            let quote = match self.options.quote_style {
                XmlQuoteStyle::Double => '"',
                XmlQuoteStyle::Single => '\'',
            };
            self.output.push('=');
            self.output.push(quote);
            self.attribute_value(&attribute.value, quote)?;
            self.output.push(quote);
        }

        if element.children.is_empty() && !self.options.expand_empty_elements {
            self.output.push_str("/>");
            return Ok(None);
        }

        self.output.push('>');
        Ok(Some(MaterializedElementFrame {
            element,
            depth,
            next_child: 0,
            block_content: self.options.indent.is_some()
                && element
                    .children
                    .iter()
                    .all(|child| !matches!(child, XmlNode::Text(_) | XmlNode::Cdata(_))),
        }))
    }

    fn text(&mut self, value: &str) -> Result<(), XmlWriteError> {
        if self.options.version == crate::XmlVersion::Xml10
            && self.options.encoding == XmlOutputEncoding::Utf8
            && self.options.escape_mode == XmlEscapeMode::Escaped
            && value
                .bytes()
                .all(|byte| (0x20..=0x7e).contains(&byte) && !matches!(byte, b'&' | b'<' | b'>'))
        {
            self.output.push_str(value);
            return Ok(());
        }
        self.validate_chars(value)?;
        if self.options.escape_mode == XmlEscapeMode::RawChecked {
            if value.contains('<') || value.contains('&') || value.contains("]]>") {
                return Err(XmlWriteError::InvalidCharacter);
            }
            self.output.push_str(value);
            return Ok(());
        }
        for character in value.chars() {
            if self.must_use_reference(character) {
                self.character_reference(character);
                continue;
            }
            match character {
                '&' => self.output.push_str("&amp;"),
                '<' => self.output.push_str("&lt;"),
                '>' => self.output.push_str("&gt;"),
                _ => self.output.push(character),
            }
        }
        Ok(())
    }

    fn attribute_value(&mut self, value: &str, quote: char) -> Result<(), XmlWriteError> {
        if self.options.version == crate::XmlVersion::Xml10
            && self.options.encoding == XmlOutputEncoding::Utf8
            && self.options.escape_mode == XmlEscapeMode::Escaped
            && value.bytes().all(|byte| {
                (0x20..=0x7e).contains(&byte)
                    && !matches!(byte, b'&' | b'<' | b'\t' | b'\n' | b'\r')
                    && byte != quote as u8
            })
        {
            self.output.push_str(value);
            return Ok(());
        }
        self.validate_chars(value)?;
        if self.options.escape_mode == XmlEscapeMode::RawChecked {
            if value.contains('<') || value.contains('&') || value.contains(quote) {
                return Err(XmlWriteError::InvalidCharacter);
            }
            self.output.push_str(value);
            return Ok(());
        }
        for character in value.chars() {
            if self.must_use_reference(character) {
                self.character_reference(character);
                continue;
            }
            match character {
                '&' => self.output.push_str("&amp;"),
                '<' => self.output.push_str("&lt;"),
                '"' if quote == '"' => self.output.push_str("&quot;"),
                '\'' if quote == '\'' => self.output.push_str("&apos;"),
                '\t' => self.output.push_str("&#x9;"),
                '\n' => self.output.push_str("&#xA;"),
                '\r' => self.output.push_str("&#xD;"),
                _ => self.output.push(character),
            }
        }
        Ok(())
    }

    fn validate_chars(&self, value: &str) -> Result<(), XmlWriteError> {
        let valid = match self.options.version {
            crate::XmlVersion::Xml10 => value.chars().all(crate::syntax::is_xml_char),
            crate::XmlVersion::Xml11 => value.chars().all(is_xml11_char),
        };
        valid.then_some(()).ok_or(XmlWriteError::InvalidCharacter)
    }

    fn must_use_reference(&self, character: char) -> bool {
        (self.options.version == crate::XmlVersion::Xml11 && !is_xml11_literal_char(character))
            || (self.options.encoding == XmlOutputEncoding::Latin1 && u32::from(character) > 0xff)
    }

    fn character_reference(&mut self, character: char) {
        self.output.push_str("&#x");
        self.output.push_str(&format!("{:X}", u32::from(character)));
        self.output.push(';');
    }

    fn comment(&mut self, value: &str) -> Result<(), XmlWriteError> {
        self.validate_literal_chars(value)?;
        if value.contains("--") || value.ends_with('-') {
            return Err(XmlWriteError::InvalidComment);
        }
        self.output.push_str("<!--");
        self.output.push_str(value);
        self.output.push_str("-->");
        Ok(())
    }

    fn cdata(&mut self, value: &str) -> Result<(), XmlWriteError> {
        self.validate_literal_chars(value)?;
        self.output.push_str("<![CDATA[");
        let mut remaining = value;
        while let Some(index) = remaining.find("]]>") {
            self.output.push_str(&remaining[..index]);
            self.output.push_str("]]]]><![CDATA[>");
            remaining = &remaining[index + 3..];
        }
        self.output.push_str(remaining);
        self.output.push_str("]]>");
        Ok(())
    }

    fn processing_instruction(
        &mut self,
        pi: &XmlProcessingInstruction,
    ) -> Result<(), XmlWriteError> {
        validate_name(&pi.target)?;
        if pi.target.eq_ignore_ascii_case("xml") || pi.data.contains("?>") {
            return Err(XmlWriteError::InvalidProcessingInstruction);
        }
        self.validate_literal_chars(&pi.data)?;
        self.output.push_str("<?");
        self.output.push_str(&pi.target);
        if !pi.data.is_empty() {
            self.output.push(' ');
            self.output.push_str(&pi.data);
        }
        self.output.push_str("?>");
        Ok(())
    }

    fn indent(&mut self, depth: usize) {
        if let Some(indent) = &self.options.indent {
            for _ in 0..depth {
                self.output.push_str(indent);
            }
        }
    }

    fn validate_literal_chars(&self, value: &str) -> Result<(), XmlWriteError> {
        self.validate_chars(value)?;
        if self.options.version == crate::XmlVersion::Xml11
            && !value.chars().all(is_xml11_literal_char)
        {
            return Err(XmlWriteError::InvalidCharacter);
        }
        Ok(())
    }
}

enum CompactOverlayChild<'a> {
    Compact { id: XmlViewNodeId },
    Materialized(&'a XmlNode),
}

fn overlay_attribute<'a>(edits: &'a SparseOverlay, path: &XmlPath, name: &str) -> Option<&'a str> {
    edits
        .attributes
        .iter()
        .find_map(|((target, candidate), value)| {
            (target == path && candidate == name).then_some(value.as_str())
        })
}

fn compact_primary<'a>(document: &'a XmlCompactDocument, record: &RawXmlNode) -> &'a str {
    compact_slice(document, record.name_start, record.name_len)
}

fn compact_secondary<'a>(document: &'a XmlCompactDocument, record: &RawXmlNode) -> &'a str {
    compact_slice(document, record.attribute_start, record.attribute_count)
}

fn compact_slice(document: &XmlCompactDocument, start: u32, len: u32) -> &str {
    let start = start as usize;
    document
        .input
        .get(start..start + len as usize)
        .expect("validated compact source range")
}

fn validate_name(name: &str) -> Result<(), XmlWriteError> {
    crate::syntax::is_valid_name(name)
        .then_some(())
        .ok_or_else(|| XmlWriteError::InvalidName(name.to_owned()))
}

fn set_declaration_value(data: &str, name: &str, value: &str) -> String {
    if let Some(name_start) = data.find(name) {
        let mut index = name_start + name.len();
        let bytes = data.as_bytes();
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if bytes.get(index) == Some(&b'=') {
            index += 1;
            while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
                index += 1;
            }
            if let Some(quote @ (b'\'' | b'"')) = bytes.get(index).copied() {
                if let Some(end_offset) = data[index + 1..].find(char::from(quote)) {
                    let end = index + 1 + end_offset;
                    let mut output = String::with_capacity(data.len() + value.len());
                    output.push_str(&data[..index + 1]);
                    output.push_str(value);
                    output.push_str(&data[end..]);
                    return output;
                }
            }
        }
    }
    if data.is_empty() {
        format!("{name}=\"{value}\"")
    } else {
        format!("{data} {name}=\"{value}\"")
    }
}

fn validate_xml_chars(value: &str) -> Result<(), XmlWriteError> {
    if value.chars().all(crate::syntax::is_xml_char) {
        Ok(())
    } else {
        Err(XmlWriteError::InvalidCharacter)
    }
}

fn validate_options(options: &XmlSerializeOptions) -> Result<(), XmlWriteError> {
    let valid_whitespace = |value: &str| {
        value
            .bytes()
            .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
    };
    if !valid_whitespace(&options.line_ending)
        || options
            .indent
            .as_deref()
            .is_some_and(|indent| !valid_whitespace(indent))
    {
        return Err(XmlWriteError::InvalidFormatting);
    }
    if options.write_bom && options.encoding == XmlOutputEncoding::Latin1 {
        return Err(XmlWriteError::InvalidFormatting);
    }
    Ok(())
}
