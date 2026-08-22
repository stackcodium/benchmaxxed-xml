mod end_tag;
mod fast_count;
mod fast_view;
mod primitives;

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
};

use crate::{
    dom::{
        CompactDocumentMetadata, RawXmlAttribute, RawXmlNode, XmlCompactDocument, XmlDoctype,
        XmlDocumentView, XmlNode, XmlNodeKind, XmlProcessingInstruction, XmlTreeStats,
        XmlViewNodeId,
    },
    dtd::{XmlGeneralEntity, parse_internal_subset_entities},
    encoding::decode_xml_bytes,
    error::{XmlError, XmlErrorKind, XmlResult},
    source::{
        XmlDocumentViewWithSourceOffsets, XmlSourceNodeId, XmlSourceOffsets,
        XmlSourceOffsetsBuilder, XmlSourceSpan,
    },
    syntax::{
        is_name_char, is_name_start_char, is_pubid_char, is_space, is_xml_char, is_xml_target,
        is_xml11_char, is_xml11_literal_char,
    },
};

const MAX_DOM_DEPTH: usize = 128;

/// Bounded policy for general entities declared in the internal subset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XmlEntityExpansionPolicy {
    /// Reject references other than the five predefined XML entities.
    Disabled,
    /// Expand internal declarations within the stated recursion and aggregate output limits.
    ExpandInternal {
        /// The max depth.
        max_depth: usize,
        /// The max expanded bytes.
        max_expanded_bytes: usize,
    },
}

impl Default for XmlEntityExpansionPolicy {
    fn default() -> Self {
        Self::ExpandInternal {
            max_depth: 16,
            max_expanded_bytes: 1024 * 1024,
        }
    }
}

/// External general entities are never loaded implicitly.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum XmlExternalEntityPolicy {
    /// Reject a reference to an external entity without filesystem or network I/O.
    #[default]
    Reject,
}

/// Policy for parsed character-data whitespace.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum XmlTextWhitespacePolicy {
    /// Preserve every text node and its normalized value.
    #[default]
    Preserve,
    /// Omit text nodes containing only XML whitespace.
    DiscardWhitespaceOnly,
    /// Trim XML whitespace at both ends and omit nodes that become empty.
    Trim,
}

/// Policy applied after XML's required attribute whitespace conversion.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum XmlAttributeWhitespacePolicy {
    /// Perform the XML-required conversion of literal whitespace to spaces.
    #[default]
    Normalize,
    /// Additionally trim and collapse XML whitespace runs to one space.
    NormalizeAndCollapse,
}

/// Options shared by strict and tolerant XML parsing entry points.
///
/// The fields are intentionally private so every option has one unambiguous meaning. Build a
/// value from [`Default::default`] or [`Self::preserve_all`] and use the fluent setters. Options
/// are `Copy`, so one policy can be reused across parsing, validation, and counting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParserConfig {
    pub(crate) preserve_declaration: bool,
    pub(crate) preserve_doctype: bool,
    pub(crate) preserve_comments: bool,
    pub(crate) preserve_processing_instructions: bool,
    pub(crate) preserve_cdata_nodes: bool,
    pub(crate) preserve_text_nodes: bool,
    pub(crate) text_whitespace: XmlTextWhitespacePolicy,
    pub(crate) attribute_whitespace: XmlAttributeWhitespacePolicy,
    pub(crate) entity_expansion: XmlEntityExpansionPolicy,
    pub(crate) external_entities: XmlExternalEntityPolicy,
    pub(crate) validate_characters: bool,
    pub(crate) validate_references: bool,
    pub(crate) validate_duplicate_attributes: bool,
}

/// The deterministic result of an explicitly tolerant parse.
///
/// `consumed_bytes` is the exclusive source boundary retained in `value`. The byte identified by
/// `diagnostic`, when it exists in the input, is not retained. Open elements whose start tags and
/// content are wholly inside that boundary are closed synthetically in the returned value.
#[derive(Clone, Debug)]
pub struct XmlParseOutcome<T> {
    /// The value.
    pub value: T,
    /// The diagnostic.
    pub diagnostic: Option<XmlError>,
    /// The consumed bytes.
    pub consumed_bytes: usize,
}

impl Default for ParserConfig {
    fn default() -> Self {
        Self {
            preserve_declaration: true,
            preserve_doctype: false,
            preserve_comments: true,
            preserve_processing_instructions: true,
            preserve_cdata_nodes: true,
            preserve_text_nodes: true,
            text_whitespace: XmlTextWhitespacePolicy::Preserve,
            attribute_whitespace: XmlAttributeWhitespacePolicy::Normalize,
            entity_expansion: XmlEntityExpansionPolicy::default(),
            external_entities: XmlExternalEntityPolicy::Reject,
            validate_characters: true,
            validate_references: true,
            validate_duplicate_attributes: true,
        }
    }
}

impl ParserConfig {
    /// Preserves every representable document node, including the doctype.
    pub const fn preserve_all() -> Self {
        Self {
            preserve_declaration: true,
            preserve_doctype: true,
            preserve_comments: true,
            preserve_processing_instructions: true,
            preserve_cdata_nodes: true,
            preserve_text_nodes: true,
            text_whitespace: XmlTextWhitespacePolicy::Preserve,
            attribute_whitespace: XmlAttributeWhitespacePolicy::Normalize,
            entity_expansion: XmlEntityExpansionPolicy::ExpandInternal {
                max_depth: 16,
                max_expanded_bytes: 1024 * 1024,
            },
            external_entities: XmlExternalEntityPolicy::Reject,
            validate_characters: true,
            validate_references: true,
            validate_duplicate_attributes: true,
        }
    }

    /// Controls whether the XML declaration is retained in materialized document representations.
    pub const fn preserve_declaration(mut self, preserve: bool) -> Self {
        self.preserve_declaration = preserve;
        self
    }

    /// Controls whether the document type declaration is retained.
    pub const fn preserve_doctype(mut self, preserve: bool) -> Self {
        self.preserve_doctype = preserve;
        self
    }

    /// Controls whether comments are represented as nodes.
    pub const fn preserve_comments(mut self, preserve: bool) -> Self {
        self.preserve_comments = preserve;
        self
    }

    /// Controls whether processing instructions are represented as nodes.
    pub const fn preserve_processing_instructions(mut self, preserve: bool) -> Self {
        self.preserve_processing_instructions = preserve;
        self
    }

    /// Controls whether CDATA sections retain distinct CDATA nodes.
    pub const fn preserve_cdata_nodes(mut self, preserve: bool) -> Self {
        self.preserve_cdata_nodes = preserve;
        self
    }

    /// Controls whether character data is represented as text nodes.
    pub const fn preserve_text_nodes(mut self, preserve: bool) -> Self {
        self.preserve_text_nodes = preserve;
        self
    }

    /// Selects the sole policy governing whitespace-only text nodes.
    pub const fn text_whitespace(mut self, policy: XmlTextWhitespacePolicy) -> Self {
        self.text_whitespace = policy;
        self
    }

    /// Selects post-XML attribute whitespace normalization.
    pub const fn attribute_whitespace(mut self, policy: XmlAttributeWhitespacePolicy) -> Self {
        self.attribute_whitespace = policy;
        self
    }

    /// Selects the bounded internal-entity expansion policy.
    pub const fn entity_expansion(mut self, policy: XmlEntityExpansionPolicy) -> Self {
        self.entity_expansion = policy;
        self
    }

    /// Selects the external-entity policy.
    pub const fn external_entities(mut self, policy: XmlExternalEntityPolicy) -> Self {
        self.external_entities = policy;
        self
    }

    /// Enables or disables XML character validation.
    pub const fn validate_characters(mut self, validate: bool) -> Self {
        self.validate_characters = validate;
        self
    }

    /// Enables or disables entity and character-reference validation.
    pub const fn validate_references(mut self, validate: bool) -> Self {
        self.validate_references = validate;
        self
    }

    /// Enables or disables duplicate-attribute validation.
    pub const fn validate_duplicate_attributes(mut self, validate: bool) -> Self {
        self.validate_duplicate_attributes = validate;
        self
    }

    /// Returns whether declarations are retained.
    pub const fn preserves_declaration(self) -> bool {
        self.preserve_declaration
    }

    /// Returns whether doctypes are retained.
    pub const fn preserves_doctype(self) -> bool {
        self.preserve_doctype
    }

    /// Returns whether comments are represented.
    pub const fn preserves_comments(self) -> bool {
        self.preserve_comments
    }

    /// Returns whether processing instructions are represented.
    pub const fn preserves_processing_instructions(self) -> bool {
        self.preserve_processing_instructions
    }

    /// Returns whether CDATA sections remain distinct nodes.
    pub const fn preserves_cdata_nodes(self) -> bool {
        self.preserve_cdata_nodes
    }

    /// Returns whether text nodes are represented.
    pub const fn preserves_text_nodes(self) -> bool {
        self.preserve_text_nodes
    }

    /// Returns the whitespace-only text-node policy.
    pub const fn text_whitespace_policy(self) -> XmlTextWhitespacePolicy {
        self.text_whitespace
    }

    /// Returns the attribute whitespace policy.
    pub const fn attribute_whitespace_policy(self) -> XmlAttributeWhitespacePolicy {
        self.attribute_whitespace
    }

    /// Returns the entity-expansion policy.
    pub const fn entity_expansion_policy(self) -> XmlEntityExpansionPolicy {
        self.entity_expansion
    }

    /// Returns the external-entity policy.
    pub const fn external_entity_policy(self) -> XmlExternalEntityPolicy {
        self.external_entities
    }

    /// Returns whether XML characters are validated.
    pub const fn validates_characters(self) -> bool {
        self.validate_characters
    }

    /// Returns whether references are validated.
    pub const fn validates_references(self) -> bool {
        self.validate_references
    }

    /// Returns whether duplicate attributes are rejected.
    pub const fn validates_duplicate_attributes(self) -> bool {
        self.validate_duplicate_attributes
    }

    #[inline]
    pub(crate) const fn effective_text_whitespace(self) -> XmlTextWhitespacePolicy {
        self.text_whitespace
    }
}

/// A reusable XML parser policy with a coherent entry point for every representation.
///
/// `XmlParser` is zero-sized beyond its copied [`ParserConfig`]. It performs no caching and can be
/// shared freely between calls. The free parsing functions are convenient one-shot entry points
/// and use either [`ParserConfig::default`] or [`ParserConfig::preserve_all`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct XmlParser {
    options: ParserConfig,
}

impl XmlParser {
    /// Creates a parser with explicit options.
    pub const fn new(options: ParserConfig) -> Self {
        Self { options }
    }

    /// Creates a parser that retains every representable document node.
    pub const fn preserving_all() -> Self {
        Self::new(ParserConfig::preserve_all())
    }

    /// Returns this parser's options.
    pub const fn options(self) -> ParserConfig {
        self.options
    }

    /// Parses an editable DOM from UTF-8 text.
    pub fn parse(&self, input: impl Into<String>) -> XmlResult<crate::XmlDom> {
        crate::XmlDom::parse_with_config(input, self.options)
    }

    /// Parses an editable DOM from encoded bytes.
    pub fn parse_bytes(&self, input: &[u8]) -> XmlResult<crate::XmlDom> {
        crate::XmlDom::parse_bytes_with_config(input, self.options)
    }

    /// Reads and parses an editable DOM.
    pub fn read(&self, input: impl std::io::Read) -> Result<crate::XmlDom, crate::XmlLoadError> {
        crate::XmlDom::read_with_config(input, self.options)
    }

    /// Loads and parses an editable DOM from a filesystem path.
    pub fn load(
        &self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<crate::XmlDom, crate::XmlLoadError> {
        crate::XmlDom::load_with_config(path, self.options)
    }

    /// Parses a compact document from UTF-8 text.
    pub fn parse_compact(&self, input: impl Into<String>) -> XmlResult<XmlCompactDocument> {
        parse_compact_document_with_config(input.into(), self.options)
    }

    /// Parses a compact document from encoded bytes.
    pub fn parse_compact_bytes(&self, input: &[u8]) -> XmlResult<XmlCompactDocument> {
        parse_compact_document_bytes_with_config(input, self.options)
    }

    /// Parses a zero-copy borrowed document view.
    pub fn parse_view<'a>(&self, input: &'a str) -> XmlResult<XmlDocumentView<'a>> {
        parse_document_view_with_config(input, self.options)
    }

    /// Parses a borrowed document view and its source-offset sidecar.
    pub fn parse_view_with_source_offsets<'a>(
        &self,
        input: &'a str,
    ) -> XmlResult<XmlDocumentViewWithSourceOffsets<'a>> {
        parse_document_view_with_config_and_source_offsets(input, self.options)
    }

    /// Parses a fragment from UTF-8 text.
    pub fn parse_fragment(&self, input: &str) -> XmlResult<crate::XmlFragment> {
        crate::parse_fragment_with_config(input, self.options)
    }

    /// Parses a recoverable DOM prefix and returns its strict diagnostic.
    pub fn parse_tolerant(
        &self,
        input: impl Into<String>,
    ) -> XmlResult<XmlParseOutcome<crate::XmlDom>> {
        crate::XmlDom::parse_tolerant_with_config(input, self.options)
    }

    /// Parses a recoverable DOM prefix from encoded bytes.
    pub fn parse_bytes_tolerant(&self, input: &[u8]) -> XmlResult<XmlParseOutcome<crate::XmlDom>> {
        crate::XmlDom::parse_bytes_tolerant_with_config(input, self.options)
    }

    /// Parses a recoverable compact-document prefix.
    pub fn parse_compact_tolerant(
        &self,
        input: impl Into<String>,
    ) -> XmlResult<XmlParseOutcome<XmlCompactDocument>> {
        parse_compact_document_tolerant_with_config(input.into(), self.options)
    }

    /// Parses a recoverable compact-document prefix from encoded bytes.
    pub fn parse_compact_bytes_tolerant(
        &self,
        input: &[u8],
    ) -> XmlResult<XmlParseOutcome<XmlCompactDocument>> {
        parse_compact_document_bytes_tolerant_with_config(input, self.options)
    }

    /// Parses a recoverable fragment prefix.
    pub fn parse_fragment_tolerant(
        &self,
        input: &str,
    ) -> XmlResult<XmlParseOutcome<crate::XmlFragment>> {
        crate::parse_fragment_tolerant_with_config(input, self.options)
    }

    /// Validates a UTF-8 document without retaining a tree.
    pub fn validate(&self, input: &str) -> XmlResult<()> {
        validate_document_with_config(input, self.options)
    }

    /// Validates an encoded document without retaining a tree.
    pub fn validate_bytes(&self, input: &[u8]) -> XmlResult<()> {
        validate_document_bytes_with_config(input, self.options)
    }

    /// Counts a UTF-8 document without retaining a tree.
    pub fn count(&self, input: &str) -> XmlResult<XmlTreeStats> {
        count_document_with_config(input, self.options)
    }

    /// Counts an encoded document without retaining a tree.
    pub fn count_bytes(&self, input: &[u8]) -> XmlResult<XmlTreeStats> {
        count_document_bytes_with_config(input, self.options)
    }
}

/// Parses a borrowed, source-backed document view.
///
/// Entity replacements containing markup are rejected because generated nodes cannot borrow
/// ranges from `input`; use [`parse_compact_document`] or [`crate::XmlDom::parse`] when those
/// replacements must be materialized.
pub fn parse_document_view(input: &str) -> XmlResult<XmlDocumentView<'_>> {
    Parser::new(input, ParserConfig::default())
        .parse_document_view()
        .map(|(view, _)| view)
}

/// Parses a borrowed, source-backed document view with an explicit policy.
///
/// As with [`parse_document_view`], markup-producing entity replacements are rejected.
pub fn parse_document_view_with_config(
    input: &str,
    config: ParserConfig,
) -> XmlResult<XmlDocumentView<'_>> {
    Parser::new(input, config)
        .parse_document_view()
        .map(|(view, _)| view)
}

/// Parses a compact document using the full XML preservation policy.
///
/// The returned document owns `input`, remains freely movable, and can be converted to the
/// [`crate::XmlDom`] facade.
pub fn parse_compact_document(input: String) -> XmlResult<XmlCompactDocument> {
    parse_compact_document_with_config(input, ParserConfig::preserve_all())
}

/// Decodes bytes and parses a compact document with the full preservation policy.
pub fn parse_compact_document_bytes(input: &[u8]) -> XmlResult<XmlCompactDocument> {
    parse_compact_document_bytes_with_config(input, ParserConfig::preserve_all())
}

/// Decodes bytes and parses a compact document with an explicit policy.
pub fn parse_compact_document_bytes_with_config(
    input: &[u8],
    config: ParserConfig,
) -> XmlResult<XmlCompactDocument> {
    let decoded = decode_xml_bytes(input)?;
    let decoded_input = decoded.input.clone().into_owned();
    parse_compact_document_with_config(decoded_input, config)
        .map_err(|error| decoded.translate_error(input, error))
}

/// Parses a compact document with an explicit policy.
pub fn parse_compact_document_with_config(
    input: String,
    config: ParserConfig,
) -> XmlResult<XmlCompactDocument> {
    let direct_root = starts_direct_compact_root(input.as_bytes());
    let input = if direct_root {
        input
    } else {
        prepare_compact_input(input, config)?
    };
    let (view, metadata, xml11) =
        Parser::new(&input, config).parse_compact_document_view(direct_root)?;
    let XmlDocumentView {
        root,
        nodes,
        attributes,
        stats,
        has_namespace_declarations,
        compact_lexemes_are_borrowable,
        compact_attribute_lexemes_are_borrowable,
        raw_source_registration,
        ..
    } = view;
    Ok(XmlCompactDocument {
        input,
        root,
        nodes,
        attributes,
        stats,
        metadata,
        config,
        xml11,
        compact_lexemes_are_borrowable,
        compact_attribute_lexemes_are_borrowable,
        default_serialization_is_source: Default::default(),
        has_namespace_declarations,
        raw_source_registration,
    })
}

/// Parses a compact document, returning a useful well-formed prefix alongside its exact strict
/// diagnostic when ordinary parsing fails.
///
/// This operation is opt-in. All existing parse functions remain strict and atomic. Resource and
/// entity-policy failures remain hard errors, as does malformed input before a root start tag is
/// recoverable.
pub fn parse_compact_document_tolerant(
    input: String,
) -> XmlResult<XmlParseOutcome<XmlCompactDocument>> {
    parse_compact_document_tolerant_with_config(input, ParserConfig::preserve_all())
}

/// Decodes bytes and performs the explicitly tolerant compact parse.
///
/// Diagnostics and `consumed_bytes` use offsets in the original encoded byte slice, including an
/// input BOM. An encoding failure itself is a hard error because no trustworthy text prefix is
/// available to the XML parser.
pub fn parse_compact_document_bytes_tolerant(
    input: &[u8],
) -> XmlResult<XmlParseOutcome<XmlCompactDocument>> {
    parse_compact_document_bytes_tolerant_with_config(input, ParserConfig::preserve_all())
}

/// Parses compact document bytes tolerant with config.
pub fn parse_compact_document_bytes_tolerant_with_config(
    input: &[u8],
    config: ParserConfig,
) -> XmlResult<XmlParseOutcome<XmlCompactDocument>> {
    let decoded = decode_xml_bytes(input)?;
    let mut outcome =
        parse_compact_document_tolerant_with_config(decoded.as_str().to_owned(), config)?;
    if let Some(diagnostic) = outcome.diagnostic.take() {
        outcome.diagnostic = Some(decoded.translate_error(input, diagnostic));
        outcome.consumed_bytes = decoded
            .translate_error(
                input,
                XmlError::new(XmlErrorKind::UnexpectedEof, outcome.consumed_bytes),
            )
            .byte;
    } else {
        outcome.consumed_bytes = input.len();
    }
    Ok(outcome)
}

/// Parses compact document tolerant with config.
pub fn parse_compact_document_tolerant_with_config(
    input: String,
    config: ParserConfig,
) -> XmlResult<XmlParseOutcome<XmlCompactDocument>> {
    match parse_compact_document_with_config(input.clone(), config) {
        Ok(value) => Ok(XmlParseOutcome {
            value,
            diagnostic: None,
            consumed_bytes: input.len(),
        }),
        Err(diagnostic) if is_hard_tolerant_failure(&diagnostic.kind) => Err(diagnostic),
        Err(diagnostic) => {
            let (recovered, consumed_bytes) =
                recover_compact_document_prefix(&input, diagnostic.byte, config)
                    .ok_or_else(|| diagnostic.clone())?;
            Ok(XmlParseOutcome {
                value: recovered,
                diagnostic: Some(diagnostic),
                consumed_bytes,
            })
        }
    }
}

pub(crate) fn recover_compact_document_prefix(
    input: &str,
    diagnostic_byte: usize,
    config: ParserConfig,
) -> Option<(XmlCompactDocument, usize)> {
    let mut cutoff = diagnostic_byte.min(input.len());
    while !input.is_char_boundary(cutoff) {
        cutoff -= 1;
    }

    let mut candidates = vec![cutoff];
    candidates.extend(
        input[..cutoff]
            .char_indices()
            .filter_map(|(index, ch)| matches!(ch, '<' | '&').then_some(index)),
    );
    candidates.sort_unstable_by(|left, right| right.cmp(left));
    candidates.dedup();

    for candidate in candidates {
        let prefix = &input[..candidate];
        let Some((open_elements, saw_root)) = scan_recoverable_prefix(prefix) else {
            continue;
        };
        if !saw_root {
            continue;
        }
        let closing_bytes = open_elements
            .iter()
            .rev()
            .map(|name| name.len() + 3)
            .sum::<usize>();
        let mut recovered = String::with_capacity(prefix.len() + closing_bytes);
        recovered.push_str(prefix);
        for name in open_elements.iter().rev() {
            recovered.push_str("</");
            recovered.push_str(name);
            recovered.push('>');
        }
        if let Ok(document) = parse_compact_document_with_config(recovered, config) {
            return Some((document, candidate));
        }
    }
    None
}

fn is_hard_tolerant_failure(kind: &XmlErrorKind) -> bool {
    matches!(
        kind,
        XmlErrorKind::DepthLimitExceeded
            | XmlErrorKind::EntityExpansionDepthLimitExceeded
            | XmlErrorKind::EntityExpansionSizeLimitExceeded
            | XmlErrorKind::EntityExpansionDisabled(_)
            | XmlErrorKind::EntityReplacementMarkupWithSourceOffsets
            | XmlErrorKind::ExternalEntityReference(_)
            | XmlErrorKind::UnsupportedEncoding(_)
    )
}

fn scan_recoverable_prefix(prefix: &str) -> Option<(Vec<String>, bool)> {
    let bytes = prefix.as_bytes();
    let mut index = 0;
    let mut open = Vec::<String>::new();
    let mut saw_root = false;
    while index < bytes.len() {
        if bytes[index] != b'<' {
            index = prefix[index..]
                .find('<')
                .map_or(bytes.len(), |relative| index + relative);
            continue;
        }
        if prefix[index..].starts_with("<!--") {
            index += prefix[index + 4..].find("-->")? + 7;
            continue;
        }
        if prefix[index..].starts_with("<![CDATA[") {
            index += prefix[index + 9..].find("]]>")? + 12;
            continue;
        }
        if prefix[index..].starts_with("<?") {
            index += prefix[index + 2..].find("?>")? + 4;
            continue;
        }
        if prefix[index..].starts_with("<!DOCTYPE") {
            index = scan_doctype_end(prefix, index)?;
            continue;
        }
        if prefix[index..].starts_with("</") {
            let end = scan_tag_end(prefix, index)?;
            let name = prefix[index + 2..end - 1]
                .trim()
                .split_ascii_whitespace()
                .next()?;
            if open.last().map(String::as_str) != Some(name) {
                return None;
            }
            open.pop();
            index = end;
            continue;
        }
        if prefix[index..].starts_with("<!") {
            return None;
        }

        let end = scan_tag_end(prefix, index)?;
        let body = prefix[index + 1..end - 1].trim();
        let empty = body.ends_with('/');
        let name = body.trim_end_matches('/').split_ascii_whitespace().next()?;
        if name.is_empty() {
            return None;
        }
        saw_root = true;
        if !empty {
            open.push(name.to_owned());
        }
        index = end;
    }
    Some((open, saw_root))
}

fn scan_tag_end(input: &str, start: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut quote = None;
    for (offset, byte) in bytes[start + 1..].iter().copied().enumerate() {
        let index = start + 1 + offset;
        match (quote, byte) {
            (Some(expected), actual) if actual == expected => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => return Some(index + 1),
            _ => {}
        }
    }
    None
}

fn scan_doctype_end(input: &str, start: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut quote = None;
    let mut subset_depth = 0usize;
    let mut index = start + 9;
    while index < bytes.len() {
        let byte = bytes[index];
        match (quote, byte) {
            (Some(expected), actual) if actual == expected => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'<') if input[index..].starts_with("<!--") => {
                index += input[index + 4..].find("-->")? + 7;
                continue;
            }
            (None, b'<') if input[index..].starts_with("<?") => {
                index += input[index + 2..].find("?>")? + 4;
                continue;
            }
            (None, b'[') => subset_depth += 1,
            (None, b']') => subset_depth = subset_depth.saturating_sub(1),
            (None, b'>') if subset_depth == 0 => return Some(index + 1),
            _ => {}
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tolerant_recovery_tests {
    use super::*;

    #[test]
    fn doctype_comment_and_pi_brackets_do_not_change_subset_depth() {
        for source in [
            "<!DOCTYPE r [<!-- ] --><!ELEMENT r ANY>]><r><ok/><broken></wrong>",
            "<!DOCTYPE r [<!-- [ --><!ELEMENT r ANY>]><r><ok/><broken></wrong>",
            "<!DOCTYPE r [<!-- ] --><!ELEMENT r ANY>]><r><name_\u{e9}/><broken></wrong>",
            "<!DOCTYPE r [<?pi ] ?><!ELEMENT r ANY>]><r><ok/><broken></wrong>",
            "<!DOCTYPE r [<?pi [ ?><!ELEMENT r ANY>]><r><name_\u{e9}/><broken></wrong>",
        ] {
            let outcome = parse_compact_document_tolerant_with_config(
                source.to_owned(),
                ParserConfig::preserve_all(),
            )
            .unwrap();
            assert_eq!(outcome.consumed_bytes, source.find("</wrong>").unwrap());
            assert!(matches!(
                outcome.diagnostic.map(|error| error.kind),
                Some(XmlErrorKind::MismatchedEndTag { expected, found })
                    if expected == "broken" && found == "wrong"
            ));
        }
    }

    #[test]
    fn borrowed_view_rejects_markup_producing_entity_replacements() {
        let body = "<!DOCTYPE r [<!ENTITY child \"<a/>\">]><r>&child;</r>";
        assert_eq!(
            parse_document_view(body).unwrap_err().kind,
            XmlErrorKind::EntityReplacementMarkupWithSourceOffsets
        );

        let attribute = "<!DOCTYPE r [<!ENTITY bad \"<a/>\">]><r a='&bad;'/>";
        assert_eq!(
            parse_document_view(attribute).unwrap_err().kind,
            XmlErrorKind::EntityReplacementMarkupWithSourceOffsets
        );
        assert_eq!(
            parse_document_view_with_source_offsets(body)
                .unwrap_err()
                .kind,
            XmlErrorKind::EntityReplacementMarkupWithSourceOffsets
        );
    }
}

fn prepare_compact_input(input: String, config: ParserConfig) -> XmlResult<String> {
    if starts_direct_compact_root(input.as_bytes()) {
        return Ok(input);
    }
    let expanded = {
        let mut parser = Parser::new(&input, config);
        parser.skip_bom();
        if parser.starts_xml_declaration() {
            parser.parse_xml_declaration()?;
        }
        parser.skip_misc()?;
        if parser.starts_with("<!DOCTYPE") {
            parser.skip_doctype()?;
            parser.skip_misc()?;
        }
        parser.validate_entity_graph()?;
        parser.expand_document_tail()?
    };
    Ok(expanded.unwrap_or(input))
}

#[inline(always)]
fn starts_direct_compact_root(input: &[u8]) -> bool {
    input.first() == Some(&b'<')
        && input
            .get(1)
            .is_some_and(|byte| !matches!(byte, b'!' | b'?'))
}

/// Parses document view with source offsets.
pub fn parse_document_view_with_source_offsets(
    input: &str,
) -> XmlResult<XmlDocumentViewWithSourceOffsets<'_>> {
    parse_document_view_with_config_and_source_offsets(input, ParserConfig::default())
}

/// Parses document view with config and source offsets.
pub fn parse_document_view_with_config_and_source_offsets(
    input: &str,
    config: ParserConfig,
) -> XmlResult<XmlDocumentViewWithSourceOffsets<'_>> {
    let (view, offsets) = Parser::new(input, config)
        .with_source_offsets()
        .parse_document_view()?;
    Ok(XmlDocumentViewWithSourceOffsets {
        view,
        offsets: offsets.expect("source offsets were enabled"),
    })
}

/// Validates document.
pub fn validate_document(input: &str) -> XmlResult<()> {
    #[cfg(debug_assertions)]
    return Parser::new(input, ParserConfig::default()).validate_document_stack_safe();
    #[cfg(not(debug_assertions))]
    Parser::new(input, ParserConfig::default()).validate_document()
}

/// Validates document with config.
pub fn validate_document_with_config(input: &str, config: ParserConfig) -> XmlResult<()> {
    #[cfg(debug_assertions)]
    return Parser::new(input, config).validate_document_stack_safe();
    #[cfg(not(debug_assertions))]
    Parser::new(input, config).validate_document()
}

/// Validates document bytes.
pub fn validate_document_bytes(input: &[u8]) -> XmlResult<()> {
    validate_document_bytes_with_config(input, ParserConfig::default())
}

/// Validates document bytes with config.
pub fn validate_document_bytes_with_config(input: &[u8], config: ParserConfig) -> XmlResult<()> {
    let result = {
        let decoded = decode_xml_bytes(input)?;
        validate_document_with_config(decoded.as_str(), config)
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) => Err(translate_decoded_error(input, error)),
    }
}

/// Counts document.
pub fn count_document(input: &str) -> XmlResult<XmlTreeStats> {
    #[cfg(debug_assertions)]
    return Parser::new(input, ParserConfig::default()).count_document_stack_safe();
    #[cfg(not(debug_assertions))]
    Parser::new(input, ParserConfig::default()).count_document()
}

/// Counts document with config.
pub fn count_document_with_config(input: &str, config: ParserConfig) -> XmlResult<XmlTreeStats> {
    #[cfg(debug_assertions)]
    return Parser::new(input, config).count_document_stack_safe();
    #[cfg(not(debug_assertions))]
    Parser::new(input, config).count_document()
}

/// Counts document bytes.
pub fn count_document_bytes(input: &[u8]) -> XmlResult<XmlTreeStats> {
    count_document_bytes_with_config(input, ParserConfig::default())
}

/// Counts document bytes with config.
pub fn count_document_bytes_with_config(
    input: &[u8],
    config: ParserConfig,
) -> XmlResult<XmlTreeStats> {
    let result = {
        let decoded = decode_xml_bytes(input)?;
        count_document_with_config(decoded.as_str(), config)
    };
    match result {
        Ok(stats) => Ok(stats),
        Err(error) => Err(translate_decoded_error(input, error)),
    }
}

#[cold]
#[inline(never)]
fn translate_decoded_error(input: &[u8], error: XmlError) -> XmlError {
    match decode_xml_bytes(input) {
        Ok(decoded) => decoded.translate_error(input, error),
        Err(_) => error,
    }
}

struct Parser<'a> {
    input: &'a str,
    bytes: &'a [u8],
    index: usize,
    config: ParserConfig,
    version: XmlVersion,
    source_offsets: Option<XmlSourceOffsetsBuilder>,
    general_entities: HashMap<String, XmlGeneralEntity>,
    expanded_entity_bytes: usize,
}

/// The XML language version selected by the declaration.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum XmlVersion {
    /// XML 1.0.
    #[default]
    Xml10,
    /// XML 1.1.
    Xml11,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str, config: ParserConfig) -> Self {
        Self {
            input,
            bytes: input.as_bytes(),
            index: 0,
            config,
            version: XmlVersion::Xml10,
            source_offsets: None,
            general_entities: HashMap::new(),
            expanded_entity_bytes: 0,
        }
    }

    fn with_source_offsets(mut self) -> Self {
        self.source_offsets = Some(XmlSourceOffsetsBuilder::new(self.input.len()));
        self
    }

    fn validate_document(mut self) -> XmlResult<()> {
        self.skip_bom();

        if self.starts_xml_declaration() {
            self.parse_xml_declaration()?;
        }
        if self.config.validate_characters {
            let _ = self.reject_invalid_chars()?;
        }

        self.skip_misc()?;

        if self.is_eof() {
            return Err(self.error(XmlErrorKind::MissingRootElement));
        }

        if self.starts_with("<!DOCTYPE") {
            self.skip_doctype()?;
            self.skip_misc()?;
        }

        self.validate_entity_graph()?;

        if let Some(expanded) = self.expand_document_tail()? {
            return Parser::new(&expanded, self.config).validate_document();
        }

        self.validate_element(1)?;
        self.skip_misc()?;

        if !self.is_eof() {
            return Err(self.error(XmlErrorKind::TrailingContent));
        }

        Ok(())
    }

    #[cfg_attr(debug_assertions, allow(dead_code))]
    fn count_document(mut self) -> XmlResult<XmlTreeStats> {
        self.skip_bom();

        if self.starts_xml_declaration() {
            self.parse_xml_declaration()?;
        }
        if self.config.validate_characters {
            let _ = self.reject_invalid_chars()?;
        }

        self.skip_misc()?;

        if self.is_eof() {
            return Err(self.error(XmlErrorKind::MissingRootElement));
        }

        if self.starts_with("<!DOCTYPE") {
            self.skip_doctype()?;
            self.skip_misc()?;
        }

        self.validate_entity_graph()?;

        if let Some(expanded) = self.expand_document_tail()? {
            return Parser::new(&expanded, self.config).count_document();
        }

        if self.is_compact_trusted_xml10() {
            let mut stats = XmlTreeStats::default();
            self.count_compact_trusted_element_xml10(&mut stats, 1)?;
            self.skip_misc()?;

            if !self.is_eof() {
                return Err(self.error(XmlErrorKind::TrailingContent));
            }

            return Ok(stats);
        }

        let mut stats = XmlTreeStats::default();
        self.count_element(&mut stats, 1)?;
        self.skip_misc()?;

        if !self.is_eof() {
            return Err(self.error(XmlErrorKind::TrailingContent));
        }

        Ok(stats)
    }

    #[cfg(debug_assertions)]
    fn validate_document_stack_safe(mut self) -> XmlResult<()> {
        self.skip_bom();

        if self.starts_xml_declaration() {
            self.parse_xml_declaration()?;
        }
        if self.config.validate_characters {
            let _ = self.reject_invalid_chars()?;
        }

        self.skip_misc()?;
        if self.is_eof() {
            return Err(self.error(XmlErrorKind::MissingRootElement));
        }
        if self.starts_with("<!DOCTYPE") {
            self.skip_doctype()?;
            self.skip_misc()?;
        }

        self.validate_entity_graph()?;
        if let Some(expanded) = self.expand_document_tail()? {
            return Parser::new(&expanded, self.config).validate_document_stack_safe();
        }

        self.validate_element_iterative()?;
        self.skip_misc()?;
        if !self.is_eof() {
            return Err(self.error(XmlErrorKind::TrailingContent));
        }
        Ok(())
    }

    #[cfg(debug_assertions)]
    fn count_document_stack_safe(mut self) -> XmlResult<XmlTreeStats> {
        self.skip_bom();

        if self.starts_xml_declaration() {
            self.parse_xml_declaration()?;
        }
        if self.config.validate_characters {
            let _ = self.reject_invalid_chars()?;
        }

        self.skip_misc()?;
        if self.is_eof() {
            return Err(self.error(XmlErrorKind::MissingRootElement));
        }
        if self.starts_with("<!DOCTYPE") {
            self.skip_doctype()?;
            self.skip_misc()?;
        }

        self.validate_entity_graph()?;
        if let Some(expanded) = self.expand_document_tail()? {
            return Parser::new(&expanded, self.config).count_document_stack_safe();
        }

        let mut stats = XmlTreeStats::default();
        self.count_element_iterative(&mut stats)?;
        self.skip_misc()?;
        if !self.is_eof() {
            return Err(self.error(XmlErrorKind::TrailingContent));
        }
        Ok(stats)
    }

    fn parse_document_view(mut self) -> XmlResult<(XmlDocumentView<'a>, Option<XmlSourceOffsets>)> {
        self.skip_bom();

        if self.starts_xml_declaration() {
            self.parse_xml_declaration()?;
        }
        let (compact_lexemes_are_borrowable, inspect_attribute_values) =
            if self.config.validate_characters {
                self.reject_invalid_chars()?
            } else {
                (false, false)
            };

        self.skip_misc()?;

        if self.is_eof() {
            return Err(self.error(XmlErrorKind::MissingRootElement));
        }

        if self.starts_with("<!DOCTYPE") {
            self.skip_doctype()?;
            self.skip_misc()?;
        }

        self.validate_entity_graph()?;
        #[cfg(debug_assertions)]
        let full_fast = self.is_full_view_xml10();
        #[cfg(not(debug_assertions))]
        let full_fast = self.source_offsets.is_none() && self.is_full_view_xml10();
        let mut builder = if full_fast && self.source_offsets.is_none() {
            XmlDocumentViewBuilder::new_dense(self.input)
        } else {
            XmlDocumentViewBuilder::new(self.input)
        };
        builder.inspect_attribute_values = inspect_attribute_values;
        let root = if self.source_offsets.is_none() && self.is_compact_trusted_xml10() {
            self.parse_compact_trusted_view_element_xml10(&mut builder)?
        } else if full_fast {
            self.parse_full_view_element_xml10(&mut builder)?
        } else {
            self.parse_view_element(&mut builder, 1)?
        };
        self.skip_misc()?;

        if !self.is_eof() {
            return Err(self.error(XmlErrorKind::TrailingContent));
        }

        let mut view = builder.finish(root);
        view.xml11 = self.version == XmlVersion::Xml11;
        view.compact_lexemes_are_borrowable = compact_lexemes_are_borrowable
            && self.config.attribute_whitespace == XmlAttributeWhitespacePolicy::Normalize;
        view.compact_attribute_lexemes_are_borrowable &= view.compact_lexemes_are_borrowable;
        let offsets = self
            .source_offsets
            .take()
            .map(XmlSourceOffsetsBuilder::finish);
        Ok((view, offsets))
    }

    fn parse_compact_document_view(
        mut self,
        direct_root: bool,
    ) -> XmlResult<(XmlDocumentView<'a>, CompactDocumentMetadata, bool)> {
        if direct_root
            && self.input.len() < 4 * 1024
            && self.config.validate_characters
            && self.config.validate_references
            && self.is_full_view_xml10()
        {
            let mut builder = XmlDocumentViewBuilder::new_dense(self.input);
            if let Some(root) = self.try_parse_simple_full_view_element_xml10(&mut builder)? {
                let trailing = &self.bytes[self.index..];
                if trailing
                    .iter()
                    .all(|byte| matches!(byte, b' ' | b'\t' | b'\n'))
                {
                    self.index = self.bytes.len();
                    let mut view = builder.finish(root);
                    view.compact_lexemes_are_borrowable = true;
                    return Ok((
                        view,
                        CompactDocumentMetadata {
                            declaration: None,
                            doctype: None,
                            misc_before_root: Vec::new(),
                            misc_after_root: Vec::new(),
                            doctype_before_misc_index: None,
                        },
                        false,
                    ));
                }
            }
            self.index = 0;
        }

        if !direct_root {
            self.skip_bom();
        }

        let declaration = if !direct_root && self.starts_xml_declaration() {
            let declaration = self.parse_xml_declaration()?;
            self.config.preserve_declaration.then_some(declaration)
        } else {
            None
        };
        let (compact_lexemes_are_borrowable, inspect_attribute_values) =
            if self.config.validate_characters {
                self.reject_invalid_chars()?
            } else {
                (false, false)
            };
        if compact_lexemes_are_borrowable {
            // The complete character scan proved that no reference opener exists anywhere in the
            // source, so the element parser can use its two-delimiter text/attribute loops without
            // weakening strict reference validation.
            self.config.validate_references = false;
        }

        let mut misc_before_root = Vec::new();
        let mut doctype = None;
        let mut doctype_before_misc_index = None;
        if !direct_root {
            if self.config.preserve_comments || self.config.preserve_processing_instructions {
                self.parse_misc(&mut misc_before_root)?;
            } else {
                self.skip_misc()?;
            }
            if self.is_eof() {
                return Err(self.error(XmlErrorKind::MissingRootElement));
            }

            if self.starts_with("<!DOCTYPE") {
                let parsed = self.parse_doctype()?;
                if self.config.preserve_doctype {
                    doctype_before_misc_index = Some(misc_before_root.len());
                    doctype = Some(parsed);
                }
                if self.config.preserve_comments || self.config.preserve_processing_instructions {
                    self.parse_misc(&mut misc_before_root)?;
                } else {
                    self.skip_misc()?;
                }
            }
            self.validate_entity_graph()?;
        }
        debug_assert!(self.expand_document_tail()?.is_none());

        let full_fast = self.is_full_view_xml10();
        let mut builder = if full_fast {
            XmlDocumentViewBuilder::new_dense(self.input)
        } else {
            XmlDocumentViewBuilder::new(self.input)
        };
        builder.inspect_attribute_values = inspect_attribute_values;
        let root = if self.is_compact_trusted_xml10() {
            self.parse_compact_trusted_view_element_xml10(&mut builder)?
        } else if full_fast {
            self.parse_full_view_element_xml10(&mut builder)?
        } else {
            self.parse_view_element(&mut builder, 1)?
        };
        let mut misc_after_root = Vec::new();
        if self.config.preserve_comments || self.config.preserve_processing_instructions {
            self.parse_misc(&mut misc_after_root)?;
        } else {
            self.skip_misc()?;
        }
        if !self.is_eof() {
            return Err(self.error(XmlErrorKind::TrailingContent));
        }

        let mut view = builder.finish(root);
        view.compact_lexemes_are_borrowable = compact_lexemes_are_borrowable
            && self.config.attribute_whitespace == XmlAttributeWhitespacePolicy::Normalize;
        view.compact_attribute_lexemes_are_borrowable &= view.compact_lexemes_are_borrowable;
        Ok((
            view,
            CompactDocumentMetadata {
                declaration,
                doctype,
                misc_before_root,
                misc_after_root,
                doctype_before_misc_index,
            },
            self.version == XmlVersion::Xml11,
        ))
    }

    fn parse_misc(&mut self, output: &mut Vec<XmlNode>) -> XmlResult<()> {
        loop {
            self.skip_whitespace();

            if self.starts_with("<!--") {
                let start = self.index;
                let comment = self.parse_comment()?;
                if self.config.preserve_comments {
                    self.push_source_leaf(XmlNodeKind::Comment, start, self.index);
                    output.push(XmlNode::Comment(comment));
                }
            } else if self.starts_with("<?") {
                let start = self.index;
                let pi = self.parse_processing_instruction()?;
                if self.config.preserve_processing_instructions {
                    self.push_source_leaf(XmlNodeKind::ProcessingInstruction, start, self.index);
                    output.push(XmlNode::ProcessingInstruction(pi));
                }
            } else {
                break;
            }
        }

        Ok(())
    }

    fn skip_misc(&mut self) -> XmlResult<()> {
        loop {
            self.skip_whitespace();

            if self.starts_with("<!--") {
                self.skip_comment()?;
            } else if self.starts_with("<?") {
                self.skip_processing_instruction()?;
            } else {
                break;
            }
        }

        Ok(())
    }

    #[cfg(debug_assertions)]
    fn validate_element_iterative(&mut self) -> XmlResult<()> {
        let mut open = [(0usize, 0usize); MAX_DOM_DEPTH];
        let mut depth = 0usize;

        loop {
            if depth == MAX_DOM_DEPTH {
                return Err(self.error(XmlErrorKind::DepthLimitExceeded));
            }
            self.expect_byte(b'<', "<")?;
            let name_start = self.index;
            let name_len = self.parse_name_slice()?.len();
            self.validate_attributes_count()?;
            self.skip_whitespace();

            if self.consume_empty_element_end() {
                if depth == 0 {
                    return Ok(());
                }
            } else {
                self.expect_byte(b'>', ">")?;
                open[depth] = (name_start, name_len);
                depth += 1;
            }

            loop {
                if self.is_eof() {
                    return Err(self.error(XmlErrorKind::UnexpectedEof));
                }
                if self.peek() == Some(b'<') {
                    match self.bytes.get(self.index + 1).copied() {
                        Some(b'/') => {
                            if depth == 0 {
                                return Err(self.error(XmlErrorKind::UnexpectedToken));
                            }
                            self.index += 2;
                            let (start, len) = open[depth - 1];
                            self.consume_end_tag_matching(&self.input[start..start + len])?;
                            depth -= 1;
                            if depth == 0 {
                                return Ok(());
                            }
                        }
                        Some(b'!') if self.starts_with("<!--") => self.skip_comment()?,
                        Some(b'!') if self.starts_with("<![CDATA[") => self.skip_cdata()?,
                        Some(b'!') => return Err(self.error(XmlErrorKind::UnexpectedToken)),
                        Some(b'?') => self.skip_processing_instruction()?,
                        Some(_) => break,
                        None => return Err(self.error(XmlErrorKind::UnexpectedEof)),
                    }
                } else {
                    self.skip_text_no_range()?;
                }
            }
        }
    }

    fn validate_element(&mut self, depth: usize) -> XmlResult<()> {
        self.expect_byte(b'<', "<")?;
        let name = self.parse_name_slice()?;
        self.validate_attributes_count()?;
        self.skip_whitespace();

        if self.consume_empty_element_end() {
            return Ok(());
        }

        self.expect_byte(b'>', ">")?;
        self.validate_content(name, depth)
    }

    #[cfg(debug_assertions)]
    fn count_element_iterative(&mut self, stats: &mut XmlTreeStats) -> XmlResult<()> {
        let mut open = [(0usize, 0usize); MAX_DOM_DEPTH];
        let mut depth = 0usize;

        loop {
            if depth == MAX_DOM_DEPTH {
                return Err(self.error(XmlErrorKind::DepthLimitExceeded));
            }
            self.expect_byte(b'<', "<")?;
            let name_start = self.index;
            let name_len = self.parse_name_slice()?.len();
            let attributes = self.validate_attributes_count()?;
            self.skip_whitespace();
            stats.elements += 1;
            stats.attributes += attributes;
            stats.nodes += 1;

            if self.consume_empty_element_end() {
                if depth == 0 {
                    return Ok(());
                }
            } else {
                self.expect_byte(b'>', ">")?;
                open[depth] = (name_start, name_len);
                depth += 1;
            }

            loop {
                if self.is_eof() {
                    return Err(self.error(XmlErrorKind::UnexpectedEof));
                }
                if self.peek() == Some(b'<') {
                    match self.bytes.get(self.index + 1).copied() {
                        Some(b'/') => {
                            if depth == 0 {
                                return Err(self.error(XmlErrorKind::UnexpectedToken));
                            }
                            self.index += 2;
                            let (start, len) = open[depth - 1];
                            self.consume_end_tag_matching(&self.input[start..start + len])?;
                            depth -= 1;
                            if depth == 0 {
                                return Ok(());
                            }
                        }
                        Some(b'!') if self.starts_with("<!--") => {
                            self.skip_comment()?;
                            if self.config.preserve_comments {
                                stats.nodes += 1;
                            }
                        }
                        Some(b'!') if self.starts_with("<![CDATA[") => {
                            let value = self.skip_cdata_range()?;
                            let has_content = value.is_some_and(|value| {
                                self.config.effective_text_whitespace()
                                    != XmlTextWhitespacePolicy::Trim
                                    || trim_source_whitespace(self.input, value, self.version).1
                                        != 0
                            });
                            if self.config.preserve_cdata_nodes
                                || (self.config.preserve_text_nodes && has_content)
                            {
                                stats.nodes += 1;
                            }
                        }
                        Some(b'!') => return Err(self.error(XmlErrorKind::UnexpectedToken)),
                        Some(b'?') => {
                            self.skip_processing_instruction()?;
                            if self.config.preserve_processing_instructions {
                                stats.nodes += 1;
                            }
                        }
                        Some(_) => break,
                        None => return Err(self.error(XmlErrorKind::UnexpectedEof)),
                    }
                } else {
                    if self.config.effective_text_whitespace() != XmlTextWhitespacePolicy::Preserve
                        && self.skip_whitespace_text()?
                    {
                        continue;
                    }
                    self.skip_text_no_range()?;
                    if self.config.preserve_text_nodes {
                        stats.nodes += 1;
                    }
                }
            }
        }
    }

    #[cfg_attr(debug_assertions, allow(dead_code))]
    fn count_element(&mut self, stats: &mut XmlTreeStats, depth: usize) -> XmlResult<()> {
        self.expect_byte(b'<', "<")?;
        let name = self.parse_name_slice()?;
        let attributes = self.validate_attributes_count()?;
        self.skip_whitespace();

        stats.elements += 1;
        stats.attributes += attributes;
        stats.nodes += 1;

        if self.consume_empty_element_end() {
            return Ok(());
        }

        self.expect_byte(b'>', ">")?;
        self.count_content(name, stats, depth)
    }

    fn validate_content(&mut self, element_name: &str, depth: usize) -> XmlResult<()> {
        loop {
            if self.is_eof() {
                return Err(self.error(XmlErrorKind::UnexpectedEof));
            }

            if self.peek() == Some(b'<') {
                match self.bytes.get(self.index + 1).copied() {
                    Some(b'/') => {
                        self.index += 2;
                        self.consume_end_tag_matching(element_name)?;
                        return Ok(());
                    }
                    Some(b'!') if self.starts_with("<!--") => self.skip_comment()?,
                    Some(b'!') if self.starts_with("<![CDATA[") => self.skip_cdata()?,
                    Some(b'!') => return Err(self.error(XmlErrorKind::UnexpectedToken)),
                    Some(b'?') => self.skip_processing_instruction()?,
                    Some(_) => {
                        if depth == MAX_DOM_DEPTH {
                            return Err(self.error(XmlErrorKind::DepthLimitExceeded));
                        }
                        self.validate_element(depth + 1)?;
                    }
                    None => return Err(self.error(XmlErrorKind::UnexpectedEof)),
                }
            } else {
                self.skip_text_no_range()?;
            }
        }
    }

    #[cfg_attr(debug_assertions, allow(dead_code))]
    fn count_content(
        &mut self,
        element_name: &str,
        stats: &mut XmlTreeStats,
        depth: usize,
    ) -> XmlResult<()> {
        loop {
            if self.is_eof() {
                return Err(self.error(XmlErrorKind::UnexpectedEof));
            }

            if self.peek() == Some(b'<') {
                match self.bytes.get(self.index + 1).copied() {
                    Some(b'/') => {
                        self.index += 2;
                        self.consume_end_tag_matching(element_name)?;
                        return Ok(());
                    }
                    Some(b'!') if self.starts_with("<!--") => {
                        self.skip_comment()?;
                        if self.config.preserve_comments {
                            stats.nodes += 1;
                        }
                    }
                    Some(b'!') if self.starts_with("<![CDATA[") => {
                        let value = self.skip_cdata_range()?;
                        let has_content = value.is_some_and(|value| {
                            self.config.effective_text_whitespace() != XmlTextWhitespacePolicy::Trim
                                || trim_source_whitespace(self.input, value, self.version).1 != 0
                        });
                        if self.config.preserve_cdata_nodes
                            || (self.config.preserve_text_nodes && has_content)
                        {
                            stats.nodes += 1;
                        }
                    }
                    Some(b'!') => return Err(self.error(XmlErrorKind::UnexpectedToken)),
                    Some(b'?') => {
                        self.skip_processing_instruction()?;
                        if self.config.preserve_processing_instructions {
                            stats.nodes += 1;
                        }
                    }
                    Some(_) => {
                        if depth == MAX_DOM_DEPTH {
                            return Err(self.error(XmlErrorKind::DepthLimitExceeded));
                        }
                        self.count_element(stats, depth + 1)?;
                    }
                    None => return Err(self.error(XmlErrorKind::UnexpectedEof)),
                }
            } else {
                if self.config.effective_text_whitespace() != XmlTextWhitespacePolicy::Preserve
                    && self.skip_whitespace_text()?
                {
                    continue;
                }
                self.skip_text_no_range()?;
                if self.config.preserve_text_nodes {
                    stats.nodes += 1;
                }
            }
        }
    }

    fn parse_view_element(
        &mut self,
        builder: &mut XmlDocumentViewBuilder<'a>,
        depth: usize,
    ) -> XmlResult<XmlViewNodeId> {
        let source_start = self.index;
        self.expect_byte(b'<', "<")?;
        let name_start = self.index;
        let name = self.parse_name_slice()?;
        let name_range = compact_range(name_start, name.len())?;
        let source_node = self.start_source_node(XmlNodeKind::Element, source_start);
        let (attribute_start, attribute_count) = self.parse_view_attributes(builder)?;
        self.skip_whitespace();

        let node = builder.push_node(
            XmlNodeKind::Element,
            name_range,
            compact_range(attribute_start, attribute_count)?,
        );
        builder.stats.elements += 1;
        builder.stats.attributes += attribute_count;
        builder.stats.nodes += 1;

        if self.consume_empty_element_end() {
            builder.close_node(node);
            self.finish_source_node(source_node, self.index);
            return Ok(node);
        }

        self.expect_byte(b'>', ">")?;
        self.parse_view_content(name, node, builder, depth)?;
        builder.close_node(node);
        self.finish_source_node(source_node, self.index);
        Ok(node)
    }

    fn parse_view_content(
        &mut self,
        element_name: &str,
        parent: XmlViewNodeId,
        builder: &mut XmlDocumentViewBuilder<'a>,
        depth: usize,
    ) -> XmlResult<()> {
        loop {
            if self.is_eof() {
                return Err(self.error(XmlErrorKind::UnexpectedEof));
            }

            if self.peek() == Some(b'<') {
                match self.bytes.get(self.index + 1).copied() {
                    Some(b'/') => {
                        self.index += 2;
                        self.consume_end_tag_matching(element_name)?;
                        return Ok(());
                    }
                    Some(b'!') if self.starts_with("<!--") => {
                        let start = self.index;
                        self.skip_comment()?;
                        if self.config.preserve_comments {
                            self.push_source_leaf(XmlNodeKind::Comment, start, self.index);
                            builder.push_leaf_child(
                                parent,
                                XmlNodeKind::Comment,
                                compact_range(start + 4, self.index - start - 7)?,
                            );
                        }
                    }
                    Some(b'!') if self.starts_with("<![CDATA[") => {
                        let start = self.index;
                        let value = self.skip_cdata_range()?;
                        if self.config.preserve_cdata_nodes {
                            self.push_source_leaf(XmlNodeKind::Cdata, start, self.index);
                            let value = if let Some(value) = value {
                                compact_range(value.0, value.1)?
                            } else {
                                (0, 0)
                            };
                            builder.push_leaf_child(parent, XmlNodeKind::Cdata, value);
                        } else if self.config.preserve_text_nodes {
                            if let Some(mut value) = value {
                                if self.config.effective_text_whitespace()
                                    == XmlTextWhitespacePolicy::Trim
                                {
                                    value = trim_source_whitespace(self.input, value, self.version);
                                    if value.1 == 0 {
                                        continue;
                                    }
                                }
                                self.push_source_leaf(XmlNodeKind::Text, start, self.index);
                                builder.push_leaf_child(
                                    parent,
                                    XmlNodeKind::Text,
                                    compact_range(value.0, value.1)?,
                                );
                            }
                        }
                    }
                    Some(b'!') => return Err(self.error(XmlErrorKind::UnexpectedToken)),
                    Some(b'?') => {
                        let start = self.index;
                        let target = self.skip_processing_instruction_target()?;
                        if self.config.preserve_processing_instructions {
                            self.push_source_leaf(
                                XmlNodeKind::ProcessingInstruction,
                                start,
                                self.index,
                            );
                            let data_start = self.skip_xml_whitespace_at(target.0 + target.1);
                            builder.push_leaf_child_with_secondary(
                                parent,
                                XmlNodeKind::ProcessingInstruction,
                                compact_range(target.0, target.1)?,
                                compact_range(data_start, self.index - 2 - data_start)?,
                            );
                        }
                    }
                    Some(_) => {
                        if depth == MAX_DOM_DEPTH {
                            return Err(self.error(XmlErrorKind::DepthLimitExceeded));
                        }
                        let child = self.parse_view_element(builder, depth + 1)?;
                        builder.link_existing_child(parent, child);
                    }
                    None => return Err(self.error(XmlErrorKind::UnexpectedEof)),
                }
            } else {
                let whitespace_policy = self.config.effective_text_whitespace();
                if whitespace_policy != XmlTextWhitespacePolicy::Preserve
                    && self.skip_whitespace_text()?
                {
                    continue;
                }
                let mut value = self.skip_text()?;
                if self.config.preserve_text_nodes {
                    if whitespace_policy == XmlTextWhitespacePolicy::Trim {
                        value = trim_source_whitespace(self.input, value, self.version);
                        if value.1 == 0 {
                            continue;
                        }
                    }
                    self.push_source_leaf(XmlNodeKind::Text, value.0, value.0 + value.1);
                    builder.push_leaf_child(
                        parent,
                        XmlNodeKind::Text,
                        compact_range(value.0, value.1)?,
                    );
                }
            }
        }
    }

    #[inline(always)]
    fn parse_view_attributes(
        &mut self,
        builder: &mut XmlDocumentViewBuilder<'a>,
    ) -> XmlResult<(usize, usize)> {
        let start = builder.attributes.len();
        let mut attribute_names = self
            .config
            .validate_duplicate_attributes
            .then(AttributeNameTracker::new);
        let mut needs_space = false;

        loop {
            let had_space = self.skip_whitespace();
            if self.peek() == Some(b'>') || self.starts_empty_element_end() {
                return Ok((start, builder.attributes.len() - start));
            }
            if needs_space && !had_space {
                return Err(self.error(XmlErrorKind::Expected("whitespace")));
            }

            let name_start = self.index;
            let name = self.parse_name_slice()?;
            builder.has_namespace_declarations |= name == "xmlns" || name.starts_with("xmlns:");
            if attribute_names
                .as_mut()
                .is_some_and(|attribute_names| !attribute_names.insert(name))
            {
                return Err(self.error(XmlErrorKind::DuplicateAttribute(name.to_owned())));
            }
            self.skip_whitespace();
            self.expect_byte(b'=', "=")?;
            self.skip_whitespace();
            let value_start = self.index + 1;
            let value = if name == "xml:space" {
                let value = self.parse_attribute_value()?;
                if value != "default" && value != "preserve" {
                    return Err(self.error(XmlErrorKind::InvalidAttributeValue));
                }
                (value_start, self.index - value_start - 1)
            } else {
                self.skip_attribute_value()?
            };
            builder.note_attribute_value(value);
            let owner = builder.attribute_source_owner();
            builder.attributes.push(RawXmlAttribute::new(
                owner,
                compact_usize(name_start)?,
                compact_usize(name.len())?,
                compact_usize(value.0)?,
                compact_usize(value.1)?,
            ));
            self.push_source_attribute(
                XmlSourceSpan {
                    start: name_start,
                    end: self.index,
                },
                XmlSourceSpan {
                    start: name_start,
                    end: name_start + name.len(),
                },
                XmlSourceSpan {
                    start: value_start,
                    end: value_start + value.1,
                },
            );
            needs_space = true;
        }
    }

    fn validate_attributes_count(&mut self) -> XmlResult<usize> {
        let mut attribute_names = self
            .config
            .validate_duplicate_attributes
            .then(AttributeNameTracker::new);
        let mut needs_space = false;
        let mut count = 0usize;

        loop {
            let had_space = self.skip_whitespace();
            if self.peek() == Some(b'>') || self.starts_empty_element_end() {
                break;
            }
            if needs_space && !had_space {
                return Err(self.error(XmlErrorKind::Expected("whitespace")));
            }

            let name = self.parse_name_slice()?;
            if attribute_names
                .as_mut()
                .is_some_and(|attribute_names| !attribute_names.insert(name))
            {
                return Err(self.error(XmlErrorKind::DuplicateAttribute(name.to_owned())));
            }
            self.skip_whitespace();
            self.expect_byte(b'=', "=")?;
            self.skip_whitespace();
            if name == "xml:space" {
                let value = self.parse_attribute_value()?;
                if value != "default" && value != "preserve" {
                    return Err(self.error(XmlErrorKind::InvalidAttributeValue));
                }
            } else {
                self.skip_attribute_value()?;
            }
            count += 1;
            needs_space = true;
        }

        Ok(count)
    }

    #[inline]
    fn start_source_node(&mut self, kind: XmlNodeKind, start: usize) -> Option<XmlSourceNodeId> {
        self.source_offsets
            .as_mut()
            .map(|offsets| offsets.start_node(kind, start))
    }

    #[inline]
    fn finish_source_node(&mut self, node: Option<XmlSourceNodeId>, end: usize) {
        if let (Some(offsets), Some(node)) = (&mut self.source_offsets, node) {
            offsets.finish_node(node, end);
        }
    }

    #[inline]
    fn push_source_leaf(&mut self, kind: XmlNodeKind, start: usize, end: usize) {
        if let Some(offsets) = &mut self.source_offsets {
            offsets.push_leaf(kind, start, end);
        }
    }

    #[inline]
    fn push_source_attribute(
        &mut self,
        span: XmlSourceSpan,
        name: XmlSourceSpan,
        value: XmlSourceSpan,
    ) {
        if let Some(offsets) = &mut self.source_offsets {
            offsets.push_attribute(span, name, value);
        }
    }

    fn resolve_runtime_entity(&mut self, name: &str) -> XmlResult<String> {
        if !self.general_entities.contains_key(name) {
            return Err(self.error(XmlErrorKind::UndeclaredEntity(name.to_owned())));
        }
        let (max_depth, max_bytes) = match self.config.entity_expansion {
            XmlEntityExpansionPolicy::Disabled => (0, 0),
            XmlEntityExpansionPolicy::ExpandInternal {
                max_depth,
                max_expanded_bytes,
            } => (max_depth, max_expanded_bytes),
        };
        let mut budget = EntityExpansionBudget {
            emitted: self.expanded_entity_bytes,
            max_depth,
            max_bytes,
        };
        let lexical = expand_one_entity(
            name,
            &self.general_entities,
            self.config.entity_expansion,
            self.config.external_entities,
            &mut budget,
            self.index,
            self.version,
        )?;
        self.expanded_entity_bytes = budget.emitted;
        if lexical.contains('<') {
            return Err(self.error(XmlErrorKind::EntityReplacementMarkupWithSourceOffsets));
        }
        decode_entity_replacement_text(&lexical, self.version, self.index)
    }

    fn validate_entity_graph(&self) -> XmlResult<()> {
        if self.general_entities.is_empty() {
            return Ok(());
        }
        validate_general_entity_graph(&self.general_entities, self.index)
    }

    fn expand_document_tail(&self) -> XmlResult<Option<String>> {
        if self.general_entities.is_empty() || !self.input.as_bytes()[self.index..].contains(&b'&')
        {
            return Ok(None);
        }
        let Some(expanded_tail) = expand_document_entity_references(
            &self.input[self.index..],
            self.index,
            &self.general_entities,
            self.config.entity_expansion,
            self.config.external_entities,
            self.version,
        )?
        else {
            return Ok(None);
        };
        let mut expanded = String::with_capacity(self.index + expanded_tail.len());
        expanded.push_str(&self.input[..self.index]);
        expanded.push_str(&expanded_tail);
        Ok(Some(expanded))
    }
}

struct EntityExpansionBudget {
    emitted: usize,
    max_depth: usize,
    max_bytes: usize,
}

fn expand_document_entity_references(
    input: &str,
    base: usize,
    entities: &HashMap<String, XmlGeneralEntity>,
    policy: XmlEntityExpansionPolicy,
    external_policy: XmlExternalEntityPolicy,
    version: XmlVersion,
) -> XmlResult<Option<String>> {
    let (max_depth, max_bytes) = match policy {
        XmlEntityExpansionPolicy::Disabled => (0, 0),
        XmlEntityExpansionPolicy::ExpandInternal {
            max_depth,
            max_expanded_bytes,
        } => (max_depth, max_expanded_bytes),
    };
    let mut budget = EntityExpansionBudget {
        emitted: 0,
        max_depth,
        max_bytes,
    };
    let mut output = String::with_capacity(input.len());
    let mut index = 0usize;
    let mut changed = false;
    let mut element_depth = 0usize;

    while index < input.len() {
        if input[index..].starts_with("<!--") {
            let end = input[index + 4..]
                .find("-->")
                .map(|offset| index + 4 + offset + 3)
                .unwrap_or(input.len());
            output.push_str(&input[index..end]);
            index = end;
            continue;
        }
        if input[index..].starts_with("<![CDATA[") {
            let end = input[index + 9..]
                .find("]]>")
                .map(|offset| index + 9 + offset + 3)
                .unwrap_or(input.len());
            output.push_str(&input[index..end]);
            index = end;
            continue;
        }
        if input[index..].starts_with("<?") {
            let end = input[index + 2..]
                .find("?>")
                .map(|offset| index + 2 + offset + 2)
                .unwrap_or(input.len());
            output.push_str(&input[index..end]);
            index = end;
            continue;
        }
        if input.as_bytes()[index] == b'<' {
            let end_tag = input[index..].starts_with("</");
            index += 1;
            output.push('<');
            while index < input.len() && input.as_bytes()[index] != b'>' {
                let byte = input.as_bytes()[index];
                if matches!(byte, b'\'' | b'"') {
                    output.push(byte as char);
                    index += 1;
                    let quote = byte;
                    while index < input.len() && input.as_bytes()[index] != quote {
                        if let Some((name, end)) = general_reference_at(input, index) {
                            if !is_predefined_entity(name) && entities.contains_key(name) {
                                let mut replacement = expand_one_entity(
                                    name,
                                    entities,
                                    policy,
                                    external_policy,
                                    &mut budget,
                                    base + index,
                                    version,
                                )?;
                                let quote_char = quote as char;
                                if replacement.contains(quote_char) {
                                    replacement = replacement.replace(
                                        quote_char,
                                        if quote == b'"' { "&quot;" } else { "&apos;" },
                                    );
                                }
                                validate_entity_boundaries(
                                    &output,
                                    &replacement,
                                    &input[end..],
                                    base + index,
                                )?;
                                output.push_str(&replacement);
                                index = end;
                                changed = true;
                                continue;
                            }
                        }
                        push_next_char(input, &mut index, &mut output);
                    }
                    if index < input.len() {
                        output.push(quote as char);
                        index += 1;
                    }
                } else {
                    push_next_char(input, &mut index, &mut output);
                }
            }
            if index < input.len() {
                let empty_tag = output[..output.len()]
                    .trim_end_matches(char::is_whitespace)
                    .ends_with('/');
                output.push('>');
                index += 1;
                if end_tag {
                    element_depth = element_depth.saturating_sub(1);
                } else if !empty_tag {
                    element_depth += 1;
                }
            }
            continue;
        }

        if let Some((name, end)) = general_reference_at(input, index) {
            if element_depth > 0 && !is_predefined_entity(name) && entities.contains_key(name) {
                let replacement = expand_one_entity(
                    name,
                    entities,
                    policy,
                    external_policy,
                    &mut budget,
                    base + index,
                    version,
                )?;
                validate_entity_boundaries(&output, &replacement, &input[end..], base + index)?;
                output.push_str(&replacement);
                index = end;
                changed = true;
                continue;
            }
        }
        push_next_char(input, &mut index, &mut output);
    }

    Ok(changed.then_some(output))
}

fn expand_one_entity(
    name: &str,
    entities: &HashMap<String, XmlGeneralEntity>,
    policy: XmlEntityExpansionPolicy,
    external_policy: XmlExternalEntityPolicy,
    budget: &mut EntityExpansionBudget,
    byte: usize,
    version: XmlVersion,
) -> XmlResult<String> {
    if policy == XmlEntityExpansionPolicy::Disabled {
        return Err(XmlError::new(
            XmlErrorKind::EntityExpansionDisabled(name.to_owned()),
            byte,
        ));
    }
    let remaining = budget.max_bytes.saturating_sub(budget.emitted);
    let mut stack = Vec::new();
    let replacement = resolve_entity_lexical(
        name,
        entities,
        external_policy,
        budget.max_depth,
        remaining,
        &mut stack,
        byte,
    )?;
    validate_entity_replacement(&replacement, byte, version)?;
    budget.emitted = budget
        .emitted
        .checked_add(replacement.len())
        .ok_or_else(|| XmlError::new(XmlErrorKind::EntityExpansionSizeLimitExceeded, byte))?;
    if budget.emitted > budget.max_bytes {
        return Err(XmlError::new(
            XmlErrorKind::EntityExpansionSizeLimitExceeded,
            byte,
        ));
    }
    Ok(replacement)
}

fn resolve_entity_lexical(
    name: &str,
    entities: &HashMap<String, XmlGeneralEntity>,
    external_policy: XmlExternalEntityPolicy,
    max_depth: usize,
    max_bytes: usize,
    stack: &mut Vec<String>,
    byte: usize,
) -> XmlResult<String> {
    if stack.len() >= max_depth || stack.iter().any(|active| active == name) {
        return Err(XmlError::new(
            XmlErrorKind::EntityExpansionDepthLimitExceeded,
            byte,
        ));
    }
    let Some(entity) = entities.get(name) else {
        return Err(XmlError::new(
            XmlErrorKind::UndeclaredEntity(name.to_owned()),
            byte,
        ));
    };
    let value = match entity {
        XmlGeneralEntity::Internal(value) => value,
        XmlGeneralEntity::External => {
            let _ = external_policy;
            return Err(XmlError::new(
                XmlErrorKind::ExternalEntityReference(name.to_owned()),
                byte,
            ));
        }
    };

    stack.push(name.to_owned());
    let mut output = String::with_capacity(value.len().min(max_bytes));
    let mut index = 0usize;
    while index < value.len() {
        if let Some(end) = opaque_entity_markup_end(value, index) {
            output.push_str(&value[index..end]);
            index = end;
            continue;
        }
        if let Some((nested, end)) = general_reference_at(value, index) {
            if !is_predefined_entity(nested) && entities.contains_key(nested) {
                let replacement = resolve_entity_lexical(
                    nested,
                    entities,
                    external_policy,
                    max_depth,
                    max_bytes.saturating_sub(output.len()),
                    stack,
                    byte,
                )?;
                output.push_str(&replacement);
                index = end;
                if output.len() > max_bytes {
                    stack.pop();
                    return Err(XmlError::new(
                        XmlErrorKind::EntityExpansionSizeLimitExceeded,
                        byte,
                    ));
                }
                continue;
            }
        }
        push_next_char(value, &mut index, &mut output);
        if output.len() > max_bytes {
            stack.pop();
            return Err(XmlError::new(
                XmlErrorKind::EntityExpansionSizeLimitExceeded,
                byte,
            ));
        }
    }
    stack.pop();
    Ok(output)
}

fn validate_general_entity_graph(
    entities: &HashMap<String, XmlGeneralEntity>,
    byte: usize,
) -> XmlResult<()> {
    let mut validated_depths = HashMap::with_capacity(entities.len());
    let mut stack = Vec::new();
    for name in entities.keys() {
        validate_general_entity_node(name, entities, &mut stack, &mut validated_depths, byte)?;
    }
    Ok(())
}

fn validate_general_entity_node<'a>(
    name: &'a str,
    entities: &'a HashMap<String, XmlGeneralEntity>,
    stack: &mut Vec<&'a str>,
    validated_depths: &mut HashMap<&'a str, usize>,
    byte: usize,
) -> XmlResult<usize> {
    if stack.len() >= 128 || stack.contains(&name) {
        return Err(XmlError::new(
            XmlErrorKind::EntityExpansionDepthLimitExceeded,
            byte,
        ));
    }
    if let Some(depth) = validated_depths.get(name).copied() {
        if stack.len().saturating_add(depth) > 128 {
            return Err(XmlError::new(
                XmlErrorKind::EntityExpansionDepthLimitExceeded,
                byte,
            ));
        }
        return Ok(depth);
    }
    let Some(entity) = entities.get(name) else {
        return Err(XmlError::new(
            XmlErrorKind::UndeclaredEntity(name.to_owned()),
            byte,
        ));
    };
    let XmlGeneralEntity::Internal(value) = entity else {
        validated_depths.insert(name, 1);
        return Ok(1);
    };
    stack.push(name);
    let mut depth = 1usize;
    let mut index = 0usize;
    while index < value.len() {
        if let Some(end) = opaque_entity_markup_end(value, index) {
            index = end;
            continue;
        }
        if let Some((nested, end)) = general_reference_at(value, index) {
            if !is_predefined_entity(nested) {
                let nested_depth =
                    validate_general_entity_node(nested, entities, stack, validated_depths, byte)?;
                depth = depth.max(nested_depth.saturating_add(1));
            }
            index = end;
        } else {
            index += value[index..].chars().next().unwrap().len_utf8();
        }
    }
    stack.pop();
    validated_depths.insert(name, depth);
    Ok(depth)
}

fn validate_entity_replacement(
    replacement: &str,
    byte: usize,
    version: XmlVersion,
) -> XmlResult<()> {
    let declaration = if version == XmlVersion::Xml11 {
        "<?xml version='1.1'?>"
    } else {
        ""
    };
    let mut wrapped = String::with_capacity(declaration.len() + replacement.len() + 17);
    wrapped.push_str(declaration);
    wrapped.push_str("<entity>");
    wrapped.push_str(replacement);
    wrapped.push_str("</entity>");
    let config = ParserConfig::default().entity_expansion(XmlEntityExpansionPolicy::Disabled);
    Parser::new(&wrapped, config)
        .validate_document()
        .map_err(|error| XmlError::new(error.kind, byte))
}

fn opaque_entity_markup_end(input: &str, index: usize) -> Option<usize> {
    for (start, end) in [("<![CDATA[", "]]>"), ("<!--", "-->"), ("<?", "?>")] {
        if input[index..].starts_with(start) {
            return input[index + start.len()..]
                .find(end)
                .map(|offset| index + start.len() + offset + end.len())
                .or(Some(input.len()));
        }
    }
    None
}

fn validate_entity_boundaries(
    before: &str,
    replacement: &str,
    after: &str,
    byte: usize,
) -> XmlResult<()> {
    let starts_reference_tail = |value: &str| {
        value
            .chars()
            .next()
            .is_some_and(|ch| ch == '#' || is_name_start_char(ch))
    };
    let creates_reference = (before.ends_with('&') && starts_reference_tail(replacement))
        || (replacement.ends_with('&') && starts_reference_tail(after));
    let mut left_boundary = String::with_capacity(4);
    left_boundary.extend(
        before
            .chars()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev(),
    );
    left_boundary.extend(replacement.chars().take(2));
    let mut right_boundary = String::with_capacity(4);
    right_boundary.extend(
        replacement
            .chars()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev(),
    );
    right_boundary.extend(after.chars().take(2));
    if creates_reference || left_boundary.contains("]]>") || right_boundary.contains("]]>") {
        return Err(XmlError::new(XmlErrorKind::InvalidDocumentStructure, byte));
    }
    Ok(())
}

fn general_reference_at(input: &str, index: usize) -> Option<(&str, usize)> {
    if input.as_bytes().get(index) != Some(&b'&')
        || matches!(input.as_bytes().get(index + 1), Some(b'#'))
    {
        return None;
    }
    let tail = input.get(index + 1..)?;
    let semicolon = tail.find(';')?;
    let name = &tail[..semicolon];
    let mut chars = name.chars();
    if !chars.next().is_some_and(is_name_start_char) || !chars.all(is_name_char) {
        return None;
    }
    Some((name, index + 1 + semicolon + 1))
}

fn is_predefined_entity(name: &str) -> bool {
    matches!(name, "amp" | "lt" | "gt" | "apos" | "quot")
}

fn push_next_char(input: &str, index: &mut usize, output: &mut String) {
    let ch = input[*index..]
        .chars()
        .next()
        .expect("index is inside source");
    output.push(ch);
    *index += ch.len_utf8();
}

fn decode_entity_replacement_text(
    input: &str,
    version: XmlVersion,
    byte: usize,
) -> XmlResult<String> {
    let mut output = String::with_capacity(input.len());
    let mut index = 0usize;
    while index < input.len() {
        if input.as_bytes()[index] != b'&' {
            let ch = input[index..].chars().next().unwrap();
            let valid = match version {
                XmlVersion::Xml10 => is_xml_char(ch),
                XmlVersion::Xml11 => is_xml11_char(ch),
            };
            if !valid {
                return Err(XmlError::new(XmlErrorKind::InvalidCharacter, byte));
            }
            output.push(ch);
            index += ch.len_utf8();
            continue;
        }

        let Some(relative_end) = input[index + 1..].find(';') else {
            return Err(XmlError::new(XmlErrorKind::UnexpectedEof, byte));
        };
        let end = index + 1 + relative_end;
        let reference = &input[index + 1..end];
        let decoded = match reference {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "apos" => '\'',
            "quot" => '"',
            value if value.starts_with("#x") => {
                decode_entity_character_reference(&value[2..], 16, version, byte)?
            }
            value if value.starts_with('#') => {
                decode_entity_character_reference(&value[1..], 10, version, byte)?
            }
            name => {
                return Err(XmlError::new(
                    XmlErrorKind::UndeclaredEntity(name.to_owned()),
                    byte,
                ));
            }
        };
        output.push(decoded);
        index = end + 1;
    }
    Ok(normalize_newlines(&output, version))
}

fn decode_entity_character_reference(
    digits: &str,
    radix: u32,
    version: XmlVersion,
    byte: usize,
) -> XmlResult<char> {
    let value = u32::from_str_radix(digits, radix)
        .ok()
        .and_then(char::from_u32)
        .filter(|ch| match version {
            XmlVersion::Xml10 => is_xml_char(*ch),
            XmlVersion::Xml11 => is_xml11_char(*ch),
        });
    value.ok_or_else(|| XmlError::new(XmlErrorKind::InvalidCharacterReference, byte))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TargetMode {
    XmlDeclaration,
    ProcessingInstruction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SegmentDelimiter {
    byte: u8,
    index: usize,
    needs_normalization: bool,
}

struct XmlDocumentViewBuilder<'a> {
    input: &'a str,
    source_owner: crate::dom::RawSourceOwner,
    nodes: Vec<RawXmlNode>,
    attributes: Vec<RawXmlAttribute>,
    stats: XmlTreeStats,
    has_namespace_declarations: bool,
    compact_attribute_lexemes_are_borrowable: bool,
    inspect_attribute_values: bool,
}

impl<'a> XmlDocumentViewBuilder<'a> {
    fn new(input: &'a str) -> Self {
        let node_capacity = (input.len() / 48).max(16);
        let attribute_capacity = (input.len() / 128).max(4);
        Self {
            input,
            source_owner: crate::dom::next_raw_source_owner(),
            nodes: Vec::with_capacity(node_capacity),
            attributes: Vec::with_capacity(attribute_capacity),
            stats: XmlTreeStats::default(),
            has_namespace_declarations: false,
            compact_attribute_lexemes_are_borrowable: true,
            inspect_attribute_values: true,
        }
    }

    fn new_dense(input: &'a str) -> Self {
        let (node_capacity, attribute_capacity) = dense_capacity_estimates(input.as_bytes());
        Self {
            input,
            source_owner: crate::dom::next_raw_source_owner(),
            nodes: Vec::with_capacity(node_capacity),
            attributes: Vec::with_capacity(attribute_capacity),
            stats: XmlTreeStats::default(),
            has_namespace_declarations: false,
            compact_attribute_lexemes_are_borrowable: true,
            inspect_attribute_values: true,
        }
    }

    #[inline(always)]
    fn push_node(
        &mut self,
        kind: XmlNodeKind,
        name: (u32, u32),
        attributes: (u32, u32),
    ) -> XmlViewNodeId {
        let id = XmlViewNodeId(self.nodes.len());
        let next = (self.nodes.len() + 1) as u32;
        self.nodes.push(RawXmlNode::new_with_owner(
            self.source_owner,
            kind,
            name.0,
            name.1,
            attributes.0,
            attributes.1,
            u32::MAX,
            next,
        ));
        id
    }

    fn push_leaf_child(
        &mut self,
        parent: XmlViewNodeId,
        kind: XmlNodeKind,
        name: (u32, u32),
    ) -> XmlViewNodeId {
        self.push_leaf_child_with_secondary(parent, kind, name, (0, 0))
    }

    fn push_leaf_child_with_secondary(
        &mut self,
        parent: XmlViewNodeId,
        kind: XmlNodeKind,
        name: (u32, u32),
        secondary: (u32, u32),
    ) -> XmlViewNodeId {
        let child = XmlViewNodeId(self.nodes.len());
        let next = (self.nodes.len() + 1) as u32;
        self.nodes.push(RawXmlNode::new_with_owner(
            self.source_owner,
            kind,
            name.0,
            name.1,
            secondary.0,
            secondary.1,
            u32::MAX,
            next,
        ));
        self.link_existing_child(parent, child);
        self.stats.nodes += 1;
        child
    }

    #[inline(always)]
    fn link_existing_child(&mut self, parent: XmlViewNodeId, child: XmlViewNodeId) {
        // Preorder construction guarantees that an element's first retained child immediately
        // follows its record. Later children can therefore skip the parent-record load entirely.
        if child.0 == parent.0 + 1 {
            self.nodes[parent.0].first_child = child.0 as u32;
        }
    }

    #[inline(always)]
    fn close_node(&mut self, node: XmlViewNodeId) {
        let next_subtree = self.nodes.len() as u32;
        self.nodes[node.0].set_element_next_subtree(next_subtree);
    }

    fn attribute_source_owner(&self) -> crate::dom::RawSourceOwner {
        self.source_owner
    }

    #[inline(always)]
    fn note_attribute_value(&mut self, value: (usize, usize)) {
        if self.inspect_attribute_values && self.compact_attribute_lexemes_are_borrowable {
            self.compact_attribute_lexemes_are_borrowable = !self.input.as_bytes()
                [value.0..value.0 + value.1]
                .iter()
                .any(|byte| matches!(byte, b'\t' | b'\n'));
        }
    }

    fn finish(self, root: XmlViewNodeId) -> XmlDocumentView<'a> {
        XmlDocumentView {
            input: self.input,
            root,
            nodes: self.nodes,
            attributes: self.attributes,
            stats: self.stats,
            has_namespace_declarations: self.has_namespace_declarations,
            xml11: false,
            compact_lexemes_are_borrowable: false,
            compact_attribute_lexemes_are_borrowable: self.compact_attribute_lexemes_are_borrowable,
            raw_source_registration: crate::dom::RawSourceRegistration::new(
                self.input,
                self.source_owner,
            ),
        }
    }
}

fn dense_capacity_estimates(input: &[u8]) -> (usize, usize) {
    const SAMPLE_BYTES: usize = 8 * 1024;
    if input.len() < 4 * 1024 {
        return ((input.len() / 15 + 1).max(16), 0);
    }
    let sample_len = input.len().min(SAMPLE_BYTES);
    if sample_len == 0 {
        return (16, 0);
    }
    let sample = &input[..sample_len];
    let (markup, equals) = sample.iter().fold((0usize, 0usize), |counts, byte| {
        (
            counts.0 + usize::from(*byte == b'<'),
            counts.1 + usize::from(*byte == b'='),
        )
    });
    let scale = |count: usize| {
        count
            .saturating_mul(input.len())
            .div_ceil(sample_len)
            .min(u32::MAX as usize)
    };
    let estimated_markup = scale(markup);
    let estimated_attributes = scale(equals);
    (
        estimated_markup.saturating_mul(3).div_ceil(2).max(16),
        estimated_attributes.saturating_mul(5).div_ceil(4),
    )
}

const INLINE_ATTRIBUTE_NAMES: usize = 9;

enum AttributeNameTracker<'a> {
    Empty,
    One(&'a str),
    Inline {
        names: [&'a str; INLINE_ATTRIBUTE_NAMES],
        len: usize,
    },
    Spilled(HashSet<&'a str>),
}

impl<'a> AttributeNameTracker<'a> {
    fn new() -> Self {
        Self::Empty
    }

    #[inline(always)]
    fn insert(&mut self, name: &'a str) -> bool {
        match self {
            Self::Empty => {
                *self = Self::One(name);
                true
            }
            Self::One(existing) => {
                if *existing == name {
                    return false;
                }
                let mut names = [""; INLINE_ATTRIBUTE_NAMES];
                names[0] = existing;
                names[1] = name;
                *self = Self::Inline { names, len: 2 };
                true
            }
            Self::Inline { names, len } => {
                if names[..*len].contains(&name) {
                    return false;
                }
                if *len < names.len() {
                    names[*len] = name;
                    *len += 1;
                    return true;
                }

                let mut spilled = HashSet::with_capacity(*len + 1);
                spilled.extend(names[..*len].iter().copied());
                let inserted = spilled.insert(name);
                debug_assert!(inserted);
                *self = Self::Spilled(spilled);
                true
            }
            Self::Spilled(names) => names.insert(name),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum CompactLexemeKind {
    Attribute,
    Text,
    Opaque,
}

pub(crate) fn decode_compact_lexeme(
    value: &str,
    kind: CompactLexemeKind,
    xml11: bool,
    attribute_whitespace: XmlAttributeWhitespacePolicy,
) -> XmlResult<String> {
    let version = if xml11 {
        XmlVersion::Xml11
    } else {
        XmlVersion::Xml10
    };
    if matches!(kind, CompactLexemeKind::Opaque) {
        return Ok(normalize_newlines(value, version));
    }

    let mut output = String::with_capacity(value.len());
    let mut index = 0usize;
    while let Some(relative) = value[index..].find('&') {
        let reference_start = index + relative;
        let literal = &value[index..reference_start];
        match kind {
            CompactLexemeKind::Attribute => {
                output.push_str(&normalize_attribute_value(literal, version));
            }
            CompactLexemeKind::Text => output.push_str(&normalize_newlines(literal, version)),
            CompactLexemeKind::Opaque => unreachable!(),
        }
        let end = value[reference_start + 1..]
            .find(';')
            .map(|relative| reference_start + 1 + relative)
            .ok_or_else(|| XmlError::new(XmlErrorKind::UnexpectedEof, reference_start))?;
        let reference = &value[reference_start + 1..end];
        let decoded = match reference {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "apos" => '\'',
            "quot" => '"',
            value if value.starts_with("#x") => {
                decode_entity_character_reference(&value[2..], 16, version, reference_start)?
            }
            value if value.starts_with('#') => {
                decode_entity_character_reference(&value[1..], 10, version, reference_start)?
            }
            name => {
                return Err(XmlError::new(
                    XmlErrorKind::UndeclaredEntity(name.to_owned()),
                    reference_start,
                ));
            }
        };
        output.push(decoded);
        index = end + 1;
    }
    let literal = &value[index..];
    match kind {
        CompactLexemeKind::Attribute => {
            output.push_str(&normalize_attribute_value(literal, version));
            if attribute_whitespace == XmlAttributeWhitespacePolicy::NormalizeAndCollapse {
                collapse_xml_whitespace(&mut output);
            }
        }
        CompactLexemeKind::Text => output.push_str(&normalize_newlines(literal, version)),
        CompactLexemeKind::Opaque => unreachable!(),
    }
    Ok(output)
}

pub(crate) fn decode_compact_lexeme_cow<'a>(
    value: &'a str,
    kind: CompactLexemeKind,
    xml11: bool,
    attribute_whitespace: XmlAttributeWhitespacePolicy,
) -> XmlResult<Cow<'a, str>> {
    if compact_lexeme_is_borrowable(value, kind, xml11, attribute_whitespace) {
        return Ok(Cow::Borrowed(value));
    }
    decode_compact_lexeme(value, kind, xml11, attribute_whitespace).map(Cow::Owned)
}

pub(crate) fn compact_lexeme_is_borrowable(
    value: &str,
    kind: CompactLexemeKind,
    xml11: bool,
    attribute_whitespace: XmlAttributeWhitespacePolicy,
) -> bool {
    let needs_newline_normalization = value.as_bytes().contains(&b'\r')
        || (xml11 && (value.contains('\u{85}') || value.contains('\u{2028}')));
    let needs_attribute_normalization = match attribute_whitespace {
        XmlAttributeWhitespacePolicy::Normalize => value
            .bytes()
            .any(|byte| matches!(byte, b'\t' | b'\n' | b'\r')),
        XmlAttributeWhitespacePolicy::NormalizeAndCollapse => {
            value.bytes().any(is_space)
                || (xml11 && (value.contains('\u{85}') || value.contains('\u{2028}')))
        }
    };
    let needs_decode = value.as_bytes().contains(&b'&')
        || match kind {
            CompactLexemeKind::Attribute => needs_attribute_normalization,
            CompactLexemeKind::Text | CompactLexemeKind::Opaque => needs_newline_normalization,
        };
    !needs_decode
}

fn normalize_newlines(value: &str, version: XmlVersion) -> String {
    match version {
        XmlVersion::Xml10 => {
            normalize_xml10_newlines_known(value, value.as_bytes().contains(&b'\r'))
        }
        XmlVersion::Xml11 => normalize_xml11_newlines_known(value, contains_xml11_newline(value)),
    }
}

fn normalize_attribute_value(value: &str, version: XmlVersion) -> String {
    match version {
        XmlVersion::Xml10 => {
            let needs_normalization = value
                .as_bytes()
                .iter()
                .any(|byte| matches!(byte, b'\t' | b'\n' | b'\r'));
            if needs_normalization {
                normalize_xml10_attribute_value_known(value, true)
            } else {
                value.to_owned()
            }
        }
        XmlVersion::Xml11 => normalize_xml11_attribute_value_known(
            value,
            value
                .chars()
                .any(|ch| matches!(ch, '\t' | '\n' | '\r' | '\u{85}' | '\u{2028}')),
        ),
    }
}

fn normalize_xml10_attribute_value_known(value: &str, needs_normalization: bool) -> String {
    if !needs_normalization
        && !value
            .as_bytes()
            .iter()
            .any(|byte| matches!(byte, b'\t' | b'\n'))
    {
        return value.to_owned();
    }

    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                output.push(' ');
            }
            '\t' | '\n' => output.push(' '),
            _ => output.push(ch),
        }
    }
    output
}

fn normalize_xml11_attribute_value_known(value: &str, needs_normalization: bool) -> String {
    if !needs_normalization {
        return value.to_owned();
    }

    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if matches!(chars.peek(), Some('\n' | '\u{85}')) {
                    chars.next();
                }
                output.push(' ');
            }
            '\t' | '\n' | '\u{85}' | '\u{2028}' => output.push(' '),
            _ => output.push(ch),
        }
    }
    output
}

fn normalize_xml10_newlines_known(value: &str, needs_normalization: bool) -> String {
    if !needs_normalization {
        return value.to_owned();
    }

    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            output.push('\n');
        } else {
            output.push(ch);
        }
    }
    output
}

fn normalize_xml11_newlines_known(value: &str, needs_normalization: bool) -> String {
    if !needs_normalization {
        return value.to_owned();
    }

    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if matches!(chars.peek(), Some('\n' | '\u{85}')) {
                    chars.next();
                }
                output.push('\n');
            }
            '\u{85}' | '\u{2028}' => output.push('\n'),
            _ => output.push(ch),
        }
    }
    output
}

fn contains_xml11_newline(value: &str) -> bool {
    value
        .as_bytes()
        .iter()
        .any(|byte| matches!(byte, b'\r' | 0xc2 | 0xe2))
}

fn parse_xml_declaration_version(data: &str) -> Option<XmlVersion> {
    let mut parser = XmlDeclarationDataParser::new(data);
    let version = parser.parse_required_attr_value("version")?;
    let version = parse_xml_version(version)?;

    let mut saw_encoding = false;
    let mut saw_standalone = false;

    loop {
        let had_space = parser.skip_spaces();
        if parser.is_eof() {
            return Some(version);
        }
        if !had_space {
            return None;
        }

        if parser.starts_with_name("encoding") {
            if saw_encoding || saw_standalone {
                return None;
            }
            saw_encoding = true;
            if !parser
                .parse_required_attr_value("encoding")
                .is_some_and(is_valid_encoding_name)
            {
                return None;
            }
        } else if parser.starts_with_name("standalone") {
            if saw_standalone {
                return None;
            }
            saw_standalone = true;
            if !parser
                .parse_required_attr_value("standalone")
                .is_some_and(|value| value == "yes" || value == "no")
            {
                return None;
            }
        } else {
            return None;
        }
    }
}

struct XmlDeclarationDataParser<'a> {
    input: &'a str,
    index: usize,
}

impl<'a> XmlDeclarationDataParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, index: 0 }
    }

    fn parse_required_attr_value(&mut self, name: &'static str) -> Option<&'a str> {
        if !self.consume_name(name) {
            return None;
        }
        self.skip_spaces();
        if !self.consume_byte(b'=') {
            return None;
        }
        self.skip_spaces();

        let quote = self.peek()?;
        if quote != b'\'' && quote != b'"' {
            return None;
        }
        self.index += 1;
        let value_start = self.index;
        while let Some(byte) = self.peek() {
            if byte == quote {
                let value = &self.input[value_start..self.index];
                self.index += 1;
                return Some(value);
            }
            if is_space(byte) || byte == b'<' || byte == b'&' {
                return None;
            }
            self.index += 1;
        }

        None
    }

    fn skip_spaces(&mut self) -> bool {
        let start = self.index;
        self.index = skip_xml_whitespace_bytes(self.input.as_bytes(), self.index);
        self.index != start
    }

    fn consume_name(&mut self, name: &str) -> bool {
        if !self.starts_with_name(name) {
            return false;
        }
        self.index += name.len();
        true
    }

    fn starts_with_name(&self, name: &str) -> bool {
        self.input[self.index..].starts_with(name)
            && !self
                .input
                .get(self.index + name.len()..)
                .and_then(|tail| tail.chars().next())
                .is_some_and(is_name_char)
    }

    fn consume_byte(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.as_bytes().get(self.index).copied()
    }

    fn is_eof(&self) -> bool {
        self.index == self.input.len()
    }
}

fn parse_xml_version(value: &str) -> Option<XmlVersion> {
    if value == "1.1" {
        return Some(XmlVersion::Xml11);
    }
    let rest = value.strip_prefix("1.")?;
    if rest.is_empty() || !rest.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(XmlVersion::Xml10)
}

fn is_valid_encoding_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[inline(always)]
fn is_ascii_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b':' | b'_')
}

#[inline(always)]
fn is_ascii_name_char(byte: u8) -> bool {
    is_ascii_name_start(byte) || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
}

#[inline(always)]
fn analyze_fast_ascii_xml_word(bytes: &[u8]) -> (u64, u64) {
    debug_assert_eq!(bytes.len(), 8);
    let chunk = u64::from_ne_bytes(bytes.try_into().unwrap());
    let high_bits = chunk & 0x8080_8080_8080_8080;
    let control_bits = chunk.wrapping_sub(0x2020_2020_2020_2020) & !chunk & 0x8080_8080_8080_8080;
    (
        high_bits | control_bits,
        contains_zero_byte(chunk ^ repeated_byte(b'&')),
    )
}

#[inline(always)]
fn analyze_fast_ascii_xml_chunk(bytes: &[u8]) -> Option<bool> {
    let (invalid, marker) = analyze_fast_ascii_xml_word(bytes);
    if invalid != 0 {
        return None;
    }
    Some(marker != 0)
}

#[inline(always)]
fn is_fast_valid_ascii_xml11_chunk(bytes: &[u8]) -> bool {
    debug_assert_eq!(bytes.len(), 8);
    let chunk = u64::from_ne_bytes(bytes.try_into().unwrap());
    let high_bits = chunk & 0x8080_8080_8080_8080;
    let zero_bits = contains_zero_byte(chunk);
    let control_bits = chunk.wrapping_sub(0x2020_2020_2020_2020) & !chunk & 0x8080_8080_8080_8080;
    let delete_bits = contains_zero_byte(chunk ^ repeated_byte(0x7f));
    high_bits == 0 && zero_bits == 0 && control_bits == 0 && delete_bits == 0
}

#[inline(always)]
fn fast_chunk_has_compact_decode_marker(bytes: &[u8]) -> bool {
    debug_assert_eq!(bytes.len(), 8);
    let chunk = u64::from_ne_bytes(bytes.try_into().unwrap());
    contains_zero_byte(chunk ^ repeated_byte(b'&')) != 0
        || contains_zero_byte(chunk ^ repeated_byte(b'\r')) != 0
}

fn compact_range(start: usize, len: usize) -> XmlResult<(u32, u32)> {
    Ok((compact_usize(start)?, compact_usize(len)?))
}

fn compact_usize(value: usize) -> XmlResult<u32> {
    u32::try_from(value).map_err(|_| XmlError::new(XmlErrorKind::InvalidDocumentStructure, value))
}

#[inline(always)]
pub(crate) fn skip_xml_whitespace_bytes(bytes: &[u8], mut index: usize) -> usize {
    while index + 8 <= bytes.len() {
        let chunk = u64::from_ne_bytes(bytes[index..index + 8].try_into().unwrap());
        let whitespace_mask = contains_zero_byte(chunk ^ repeated_byte(b' '))
            | contains_zero_byte(chunk ^ repeated_byte(b'\t'))
            | contains_zero_byte(chunk ^ repeated_byte(b'\r'))
            | contains_zero_byte(chunk ^ repeated_byte(b'\n'));
        if whitespace_mask != 0x8080_8080_8080_8080 {
            break;
        }
        index += 8;
    }

    while matches!(bytes.get(index), Some(byte) if is_space(*byte)) {
        index += 1;
    }

    index
}

#[inline(always)]
fn skip_xml11_whitespace_bytes(bytes: &[u8], mut index: usize) -> usize {
    loop {
        index = skip_xml_whitespace_bytes(bytes, index);
        if bytes.get(index) == Some(&0xc2) && bytes.get(index + 1) == Some(&0x85) {
            index += 2;
            continue;
        }
        if bytes.get(index) == Some(&0xe2)
            && bytes.get(index + 1) == Some(&0x80)
            && bytes.get(index + 2) == Some(&0xa8)
        {
            index += 3;
            continue;
        }
        return index;
    }
}

#[inline(always)]
fn is_xml11_space_at(bytes: &[u8], index: usize) -> bool {
    matches!(bytes.get(index), Some(byte) if is_space(*byte))
        || (bytes.get(index) == Some(&0xc2) && bytes.get(index + 1) == Some(&0x85))
        || (bytes.get(index) == Some(&0xe2)
            && bytes.get(index + 1) == Some(&0x80)
            && bytes.get(index + 2) == Some(&0xa8))
}

#[inline(always)]
fn find_byte4(
    bytes: &[u8],
    mut index: usize,
    first: u8,
    second: u8,
    third: u8,
    fourth: u8,
) -> Option<(usize, u8)> {
    let first_pattern = repeated_byte(first);
    let second_pattern = repeated_byte(second);
    let third_pattern = repeated_byte(third);
    let fourth_pattern = repeated_byte(fourth);
    while index + 32 <= bytes.len() {
        for offset in [0usize, 8, 16, 24] {
            let start = index + offset;
            let chunk = u64::from_ne_bytes(bytes[start..start + 8].try_into().unwrap());
            let mask = contains_zero_byte(chunk ^ first_pattern)
                | contains_zero_byte(chunk ^ second_pattern)
                | contains_zero_byte(chunk ^ third_pattern)
                | contains_zero_byte(chunk ^ fourth_pattern);
            if mask != 0 {
                let found = start + ((mask.trailing_zeros() >> 3) as usize);
                return Some((found, bytes[found]));
            }
        }
        index += 32;
    }
    while index + 8 <= bytes.len() {
        let chunk = u64::from_ne_bytes(bytes[index..index + 8].try_into().unwrap());
        let mask = contains_zero_byte(chunk ^ first_pattern)
            | contains_zero_byte(chunk ^ second_pattern)
            | contains_zero_byte(chunk ^ third_pattern)
            | contains_zero_byte(chunk ^ fourth_pattern);
        if mask != 0 {
            let found = index + ((mask.trailing_zeros() >> 3) as usize);
            return Some((found, bytes[found]));
        }
        index += 8;
    }

    while let Some(byte) = bytes.get(index).copied() {
        if byte == first || byte == second || byte == third || byte == fourth {
            return Some((index, byte));
        }
        index += 1;
    }

    None
}

#[inline(always)]
fn find_byte2(bytes: &[u8], mut index: usize, first: u8, second: u8) -> Option<(usize, u8)> {
    let first_pattern = repeated_byte(first);
    let second_pattern = repeated_byte(second);
    while index + 32 <= bytes.len() {
        for offset in [0usize, 8, 16, 24] {
            let start = index + offset;
            let chunk = u64::from_ne_bytes(bytes[start..start + 8].try_into().unwrap());
            let mask = contains_zero_byte(chunk ^ first_pattern)
                | contains_zero_byte(chunk ^ second_pattern);
            if mask != 0 {
                let found = start + ((mask.trailing_zeros() >> 3) as usize);
                return Some((found, bytes[found]));
            }
        }
        index += 32;
    }
    while index + 8 <= bytes.len() {
        let chunk = u64::from_ne_bytes(bytes[index..index + 8].try_into().unwrap());
        let mask =
            contains_zero_byte(chunk ^ first_pattern) | contains_zero_byte(chunk ^ second_pattern);
        if mask != 0 {
            let found = index + ((mask.trailing_zeros() >> 3) as usize);
            return Some((found, bytes[found]));
        }
        index += 8;
    }

    while let Some(byte) = bytes.get(index).copied() {
        if byte == first || byte == second {
            return Some((index, byte));
        }
        index += 1;
    }

    None
}

#[inline(always)]
fn repeated_byte(byte: u8) -> u64 {
    u64::from_ne_bytes([byte; 8])
}

#[inline(always)]
fn contains_zero_byte(word: u64) -> u64 {
    word.wrapping_sub(0x0101_0101_0101_0101) & !word & 0x8080_8080_8080_8080
}

fn collapse_xml_whitespace(value: &mut String) {
    let mut output = String::with_capacity(value.len());
    let mut pending_space = false;
    for ch in value.chars() {
        if is_xml_whitespace_char(ch) {
            pending_space = !output.is_empty();
        } else {
            if pending_space {
                output.push(' ');
            }
            output.push(ch);
            pending_space = false;
        }
    }
    *value = output;
}

fn trim_source_whitespace(
    input: &str,
    value: (usize, usize),
    version: XmlVersion,
) -> (usize, usize) {
    let source = &input[value.0..value.0 + value.1];
    let trimmed_start = source.trim_start_matches(|ch| is_trim_whitespace(ch, version));
    let leading = source.len() - trimmed_start.len();
    let trimmed = trimmed_start.trim_end_matches(|ch| is_trim_whitespace(ch, version));
    (value.0 + leading, trimmed.len())
}

fn is_trim_whitespace(ch: char, version: XmlVersion) -> bool {
    is_xml_whitespace_char(ch)
        || (version == XmlVersion::Xml11 && matches!(ch, '\u{85}' | '\u{2028}'))
}

fn is_xml_whitespace_char(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\r')
}
