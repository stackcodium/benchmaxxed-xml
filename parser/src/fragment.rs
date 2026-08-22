use std::{ops::Range, str::FromStr};

use crate::{
    ParserConfig, XmlElement, XmlError, XmlErrorKind, XmlMemoryRetention, XmlNode, XmlParseOutcome,
    XmlResult, parse_compact_document_tolerant_with_config, parse_compact_document_with_config,
};

const FRAGMENT_WRAPPER_STEM: &str = "xml-fragment-";

fn wrap_fragment(source: &str) -> (String, usize, String) {
    let mut wrapper_name = format!("{FRAGMENT_WRAPPER_STEM}{}", source.len());
    let longest_suffix = source
        .match_indices(&wrapper_name)
        .map(|(start, matched)| {
            source.as_bytes()[start + matched.len()..]
                .iter()
                .take_while(|byte| **byte == b'_')
                .count()
        })
        .max();
    if let Some(longest_suffix) = longest_suffix {
        wrapper_name.extend(std::iter::repeat_n('_', longest_suffix + 1));
    }
    debug_assert!(!source.contains(&wrapper_name));

    let prefix_len = wrapper_name.len() + 2;
    let mut wrapped = String::with_capacity(source.len() + wrapper_name.len() * 2 + 5);
    wrapped.push('<');
    wrapped.push_str(&wrapper_name);
    wrapped.push('>');
    wrapped.push_str(source);
    wrapped.push_str("</");
    wrapped.push_str(&wrapper_name);
    wrapped.push('>');
    (wrapped, prefix_len, wrapper_name)
}

fn translate_fragment_error(
    mut error: XmlError,
    source: &str,
    prefix_len: usize,
    wrapper_name: &str,
) -> XmlError {
    error.byte = error.byte.saturating_sub(prefix_len).min(source.len());
    if matches!(
        &error.kind,
        XmlErrorKind::MismatchedEndTag { expected, .. } if expected == wrapper_name
    ) {
        error.byte = source[..error.byte].rfind("</").unwrap_or(error.byte);
    }
    error
}

/// A sequence of nodes parsed without requiring a document element.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct XmlFragment {
    nodes: Vec<XmlNode>,
}

impl XmlFragment {
    /// Returns nodes.
    pub fn nodes(&self) -> &[XmlNode] {
        &self.nodes
    }

    /// Converts this value with `into_nodes`.
    pub fn into_nodes(self) -> Vec<XmlNode> {
        self.nodes
    }
}

impl FromStr for XmlFragment {
    type Err = XmlError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        parse_fragment(source)
    }
}

/// Parses zero or more top-level fragment nodes, including character data.
pub fn parse_fragment(source: &str) -> XmlResult<XmlFragment> {
    parse_fragment_with_config(source, ParserConfig::default())
}

/// Parses fragment with config.
pub fn parse_fragment_with_config(source: &str, config: ParserConfig) -> XmlResult<XmlFragment> {
    let (wrapped, prefix_len, wrapper_name) = wrap_fragment(source);
    let compact = parse_compact_document_with_config(wrapped, config)
        .map_err(|error| translate_fragment_error(error, source, prefix_len, &wrapper_name))?;
    let mut root = compact.materialize_element(compact.root())?;
    Ok(XmlFragment {
        nodes: std::mem::take(&mut root.children),
    })
}

/// Parses a useful fragment prefix while preserving the exact strict diagnostic.
///
/// Open elements in the retained prefix are closed synthetically. `consumed_bytes` is an
/// exclusive offset in `source`; strict fragment parsing remains atomic and unchanged.
pub fn parse_fragment_tolerant(source: &str) -> XmlResult<XmlParseOutcome<XmlFragment>> {
    parse_fragment_tolerant_with_config(source, ParserConfig::default())
}

/// Parses fragment tolerant with config.
pub fn parse_fragment_tolerant_with_config(
    source: &str,
    config: ParserConfig,
) -> XmlResult<XmlParseOutcome<XmlFragment>> {
    let (wrapped, prefix_len, wrapper_name) = wrap_fragment(source);
    let outcome = parse_compact_document_tolerant_with_config(wrapped, config)?;
    let mut root = outcome.value.materialize_element(outcome.value.root())?;
    let diagnostic = outcome
        .diagnostic
        .map(|error| translate_fragment_error(error, source, prefix_len, &wrapper_name));
    let consumed_bytes = if diagnostic.is_some() {
        outcome
            .consumed_bytes
            .saturating_sub(prefix_len)
            .min(source.len())
    } else {
        source.len()
    };
    Ok(XmlParseOutcome {
        value: XmlFragment {
            nodes: std::mem::take(&mut root.children),
        },
        diagnostic,
        consumed_bytes,
    })
}

impl XmlElement {
    /// Appends fragment nodes, retaining spare child-vector capacity for later edits.
    pub fn append_fragment(&mut self, fragment: XmlFragment) -> Range<usize> {
        self.append_fragment_with_retention(fragment, XmlMemoryRetention::RetainCapacity)
    }

    /// Appends fragment nodes with explicit destination-vector capacity behavior.
    pub fn append_fragment_with_retention(
        &mut self,
        fragment: XmlFragment,
        retention: XmlMemoryRetention,
    ) -> Range<usize> {
        let start = self.children.len();
        self.children.extend(fragment.nodes);
        if retention == XmlMemoryRetention::ReleaseSpareCapacity {
            self.children.shrink_to_fit();
        }
        start..self.children.len()
    }

    /// Parses and atomically appends XML fragment source.
    pub fn append_xml(&mut self, source: &str) -> XmlResult<Range<usize>> {
        self.append_xml_with_config(source, ParserConfig::default())
    }

    /// Appends xml with config.
    pub fn append_xml_with_config(
        &mut self,
        source: &str,
        config: ParserConfig,
    ) -> XmlResult<Range<usize>> {
        let fragment = parse_fragment_with_config(source, config)?;
        Ok(self.append_fragment(fragment))
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ParserConfig, XmlElement, XmlMemoryRetention, XmlNode, parse_fragment,
        parse_fragment_tolerant, parse_fragment_with_config,
    };

    #[test]
    fn parses_multiple_nodes_and_optional_character_data() {
        let fragment = parse_fragment("text<a/><!--c--><?go now?><b/>tail").unwrap();
        assert_eq!(fragment.nodes().len(), 6);
        assert!(matches!(fragment.nodes()[0], XmlNode::Text(ref value) if value == "text"));
        assert!(matches!(fragment.nodes()[5], XmlNode::Text(ref value) if value == "tail"));

        let fragment = parse_fragment_with_config(
            "text<a>nested</a>",
            ParserConfig::default().preserve_text_nodes(false),
        )
        .unwrap();
        assert_eq!(fragment.nodes().len(), 1);
        assert_eq!(fragment.nodes()[0].as_element().unwrap().children.len(), 0);
    }

    #[test]
    fn append_is_atomic_and_returns_the_inserted_range() {
        let mut root = XmlElement::new("r").unwrap();
        root.append_element("existing").unwrap();
        let range = root.append_xml("<a/>text<b/>").unwrap();
        assert_eq!(range, 1..4);
        let snapshot = root.clone();
        assert!(root.append_xml("<broken>").is_err());
        assert_eq!(root, snapshot);
    }

    #[test]
    fn fragment_errors_use_fragment_source_offsets() {
        let error = parse_fragment("<a></b>").unwrap_err();
        assert_eq!(error.byte, 7);
        let error = parse_fragment("<a>").unwrap_err();
        assert_eq!(error.byte, 3);
    }

    #[test]
    fn fragment_wrapper_syntax_never_leaks_into_caller_semantics() {
        for source in [
            "</xml-fragment-18>",
            "</xml-fragment-19_>",
            "</xml-fragment><xml-fragment>",
            "</xml-fragment-37><xml-fragment-37>",
            "</xml-fragment-39_><xml-fragment-39_>",
            "</xml-fragment-41__><xml-fragment-41__>",
        ] {
            let strict = parse_fragment(source).unwrap_err();
            assert_eq!(strict.byte, 0, "{source}: {strict:?}");
            let tolerant = parse_fragment_tolerant(source).unwrap();
            assert_eq!(tolerant.consumed_bytes, 0, "{source}");
            assert!(tolerant.value.nodes().is_empty(), "{source}");
            assert_eq!(tolerant.diagnostic.as_ref(), Some(&strict), "{source}");
        }

        let fragment = parse_fragment(
            "<!-- </xml-fragment-61> --><![CDATA[ </xml-fragment-61> ]]><?p xml-fragment-61?>",
        )
        .unwrap();
        assert_eq!(fragment.nodes().len(), 3);
        assert!(
            matches!(&fragment.nodes()[0], XmlNode::Comment(value) if value.contains("xml-fragment"))
        );
        assert!(
            matches!(&fragment.nodes()[1], XmlNode::Cdata(value) if value.contains("xml-fragment"))
        );
        assert!(
            matches!(&fragment.nodes()[2], XmlNode::ProcessingInstruction(value) if value.data.contains("xml-fragment"))
        );

        let source = "é<a>ok</wrong>";
        let strict = parse_fragment(source).unwrap_err();
        assert_eq!(strict.byte, source.len());
        let tolerant = parse_fragment_tolerant(source).unwrap();
        assert_eq!(tolerant.diagnostic.as_ref(), Some(&strict));
        assert_eq!(tolerant.consumed_bytes, source.find("</wrong>").unwrap());
    }

    #[test]
    fn tolerant_fragment_returns_complete_nodes_and_closes_the_open_prefix() {
        let source = "text<a>ok</wrong><after/>";
        let strict = parse_fragment(source).unwrap_err();
        let outcome = parse_fragment_tolerant(source).unwrap();
        assert_eq!(outcome.diagnostic.as_ref(), Some(&strict));
        assert_eq!(strict.byte, 17);
        assert_eq!(outcome.consumed_bytes, 9);
        assert_eq!(outcome.value.nodes().len(), 2);
        assert!(matches!(&outcome.value.nodes()[0], XmlNode::Text(value) if value == "text"));
        let element = outcome.value.nodes()[1].as_element().unwrap();
        assert_eq!(element.name, "a");
        assert_eq!(element.text(), Some("ok"));
        assert!(parse_fragment(source).is_err());
    }

    #[test]
    fn lifecycle_helpers_make_capacity_retention_explicit() {
        let mut root = XmlElement::new("r").unwrap();
        root.append_element("old").unwrap();
        root.children.reserve(32);
        let reserved = root.children.capacity();
        root.append_fragment(parse_fragment("<a/><b/>").unwrap());
        assert_eq!(root.children.capacity(), reserved);

        let removed = root.remove_child_at(0).unwrap();
        assert!(matches!(removed, XmlNode::Element(_)));
        assert_eq!(root.children.capacity(), reserved);
        root.remove_child_at_with_retention(0, XmlMemoryRetention::ReleaseSpareCapacity);
        assert!(root.children.capacity() <= reserved);
    }
}
