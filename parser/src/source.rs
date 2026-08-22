use std::ops::Range;

use crate::{XmlDocumentView, XmlNodeKind, XmlPath, XmlViewNodeId};

/// A half-open byte range in the original UTF-8 source string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XmlSourceSpan {
    /// The start.
    pub start: usize,
    /// The end.
    pub end: usize,
}

impl XmlSourceSpan {
    /// Converts the span to a standard half-open range.
    pub fn as_range(self) -> Range<usize> {
        self.start..self.end
    }

    /// Returns the span length in source bytes.
    pub fn len(self) -> usize {
        self.end - self.start
    }

    /// Returns whether this value is empty.
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// An identifier into an opt-in [`XmlSourceOffsets`] sidecar.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct XmlSourceNodeId(pub(crate) usize);

impl XmlSourceNodeId {
    /// Returns the underlying document-order index.
    pub fn index(self) -> usize {
        self.0
    }
}

/// Source metadata for one preserved node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XmlSourceNode {
    /// The kind.
    pub kind: XmlNodeKind,
    /// The span.
    pub span: XmlSourceSpan,
    /// The parent.
    pub parent: Option<XmlSourceNodeId>,
}

/// Source metadata for one attribute, including its quotes in `span`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XmlSourceAttribute {
    /// The span.
    pub span: XmlSourceSpan,
    /// The name.
    pub name: XmlSourceSpan,
    /// The value.
    pub value: XmlSourceSpan,
    /// The parent.
    pub parent: XmlSourceNodeId,
}

/// Opt-in source metadata stored separately from compact DOM records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XmlSourceOffsets {
    input_len: usize,
    root: XmlSourceNodeId,
    nodes: Vec<XmlSourceNode>,
    attributes: Vec<XmlSourceAttribute>,
}

impl XmlSourceOffsets {
    /// Returns the original UTF-8 input length in bytes.
    pub fn input_len(&self) -> usize {
        self.input_len
    }

    /// Returns the source record for the document element.
    pub fn root(&self) -> XmlSourceNodeId {
        self.root
    }

    /// Returns all node source records in document order.
    pub fn nodes(&self) -> &[XmlSourceNode] {
        &self.nodes
    }

    /// Returns all attribute source records in document order.
    pub fn attributes(&self) -> &[XmlSourceAttribute] {
        &self.attributes
    }

    /// Returns the source record identified by `id`.
    pub fn node(&self, id: XmlSourceNodeId) -> Option<&XmlSourceNode> {
        self.nodes.get(id.0)
    }

    /// Resolves a logical element path into its original source record.
    pub fn node_at_path(&self, path: &XmlPath) -> Option<XmlSourceNodeId> {
        let mut current = self.root;
        for &child_index in path.indexes() {
            current = self
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, node)| node.parent == Some(current))
                .nth(child_index)
                .map(|(index, _)| XmlSourceNodeId(index))?;
        }
        Some(current)
    }

    /// Returns the source attributes belonging to a node in document order.
    pub fn attributes_of(
        &self,
        node: XmlSourceNodeId,
    ) -> impl Iterator<Item = &XmlSourceAttribute> {
        self.attributes
            .iter()
            .filter(move |attribute| attribute.parent == node)
    }
}

/// A borrowed compact view paired with opt-in source metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XmlDocumentViewWithSourceOffsets<'a> {
    /// The view.
    pub view: XmlDocumentView<'a>,
    /// The offsets.
    pub offsets: XmlSourceOffsets,
}

impl<'a> XmlDocumentViewWithSourceOffsets<'a> {
    /// Compact view node IDs and source node IDs have the same document-order index.
    pub fn node_span(&self, node: XmlViewNodeId) -> Option<XmlSourceSpan> {
        self.offsets.nodes.get(node.index()).map(|node| node.span)
    }

    /// Compact attribute indexes and source attribute indexes have the same order.
    pub fn attribute_source(&self, index: usize) -> Option<&XmlSourceAttribute> {
        self.offsets.attributes.get(index)
    }

    /// Converts this value with `into_parts`.
    pub fn into_parts(self) -> (XmlDocumentView<'a>, XmlSourceOffsets) {
        (self.view, self.offsets)
    }
}

pub(crate) struct XmlSourceOffsetsBuilder {
    input_len: usize,
    root: Option<XmlSourceNodeId>,
    nodes: Vec<XmlSourceNode>,
    attributes: Vec<XmlSourceAttribute>,
    parents: Vec<XmlSourceNodeId>,
}

impl XmlSourceOffsetsBuilder {
    pub(crate) fn new(input_len: usize) -> Self {
        Self {
            input_len,
            root: None,
            nodes: Vec::new(),
            attributes: Vec::new(),
            parents: Vec::new(),
        }
    }

    pub(crate) fn start_node(&mut self, kind: XmlNodeKind, start: usize) -> XmlSourceNodeId {
        let id = XmlSourceNodeId(self.nodes.len());
        let parent = self.parents.last().copied();
        if kind == XmlNodeKind::Element && parent.is_none() && self.root.is_none() {
            self.root = Some(id);
        }
        self.nodes.push(XmlSourceNode {
            kind,
            span: XmlSourceSpan { start, end: start },
            parent,
        });
        self.parents.push(id);
        id
    }

    pub(crate) fn finish_node(&mut self, id: XmlSourceNodeId, end: usize) {
        debug_assert_eq!(self.parents.pop(), Some(id));
        self.nodes[id.0].span.end = end;
    }

    pub(crate) fn push_leaf(
        &mut self,
        kind: XmlNodeKind,
        start: usize,
        end: usize,
    ) -> XmlSourceNodeId {
        let id = XmlSourceNodeId(self.nodes.len());
        if kind == XmlNodeKind::Element && self.parents.is_empty() && self.root.is_none() {
            self.root = Some(id);
        }
        self.nodes.push(XmlSourceNode {
            kind,
            span: XmlSourceSpan { start, end },
            parent: self.parents.last().copied(),
        });
        id
    }

    pub(crate) fn push_attribute(
        &mut self,
        span: XmlSourceSpan,
        name: XmlSourceSpan,
        value: XmlSourceSpan,
    ) {
        let parent = *self
            .parents
            .last()
            .expect("attributes are recorded while an element is open");
        self.attributes.push(XmlSourceAttribute {
            span,
            name,
            value,
            parent,
        });
    }

    pub(crate) fn finish(self) -> XmlSourceOffsets {
        debug_assert!(self.parents.is_empty());
        XmlSourceOffsets {
            input_len: self.input_len,
            root: self.root.expect("a parsed XML document has a root element"),
            nodes: self.nodes,
            attributes: self.attributes,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::mem::size_of;

    use crate::{RawXmlAttribute, RawXmlNode, parse_document_view_with_source_offsets};

    #[test]
    fn borrowed_offsets_align_without_growing_compact_records() {
        let input = "<!--skip--><root a='1'>x<child/><![CDATA[y]]></root>";
        let parsed = parse_document_view_with_source_offsets(input).unwrap();
        for node in parsed.view.node_ids() {
            let span = parsed.node_span(node).unwrap();
            assert!(!input[span.as_range()].is_empty());
        }
        let attribute = parsed.attribute_source(0).unwrap();
        assert_eq!(&input[attribute.span.as_range()], "a='1'");

        assert_eq!(size_of::<RawXmlNode>(), 28);
        assert_eq!(size_of::<RawXmlAttribute>(), 20);
    }

    #[test]
    fn copied_raw_attribute_rejects_reused_and_foreign_source_storage() {
        const ORIGINAL: &str = "<r a='ORIGINAL'/>";
        const FOREIGN: &str = "<r a='FOREIGN!'/>";
        let attribute;
        let original_pointer;
        {
            let source = String::from(ORIGINAL);
            original_pointer = source.as_ptr();
            let view = crate::parse_document_view(&source).unwrap();
            attribute = *view.attributes().first().unwrap();
            assert_eq!(attribute.name(view.raw_source()), Some("a"));
            assert_eq!(attribute.value(view.raw_source()), Some("ORIGINAL"));
        }

        let mut reuse_count = 0;
        for _ in 0..4096 {
            let foreign = String::from(FOREIGN);
            let view = crate::parse_document_view(&foreign).unwrap();
            if foreign.as_ptr() == original_pointer {
                reuse_count += 1;
                assert_eq!(attribute.name(view.raw_source()), None);
                assert_eq!(attribute.value(view.raw_source()), None);
            }
        }
        assert!(reuse_count > 0, "fixture allocator did not reuse storage");

        let compact = crate::parse_compact_document(ORIGINAL.to_owned()).unwrap();
        let compact_attribute = *compact.attributes().first().unwrap();
        assert_eq!(compact_attribute.name(compact.raw_source()), Some("a"));
        assert_eq!(
            compact_attribute.value(compact.raw_source()),
            Some("ORIGINAL")
        );
        let cloned = compact.clone();
        assert_eq!(compact_attribute.name(cloned.raw_source()), Some("a"));
        assert_eq!(
            compact_attribute.value(cloned.raw_source()),
            Some("ORIGINAL")
        );
    }

    #[test]
    fn copied_raw_node_rejects_reused_and_foreign_source_storage() {
        const ORIGINAL: &str = "<r>ORIGINAL</r>";
        const FOREIGN: &str = "<r>FOREIGN!</r>";
        let node;
        let original_pointer;
        {
            let source = String::from(ORIGINAL);
            original_pointer = source.as_ptr();
            let view = crate::parse_document_view(&source).unwrap();
            node = *view
                .nodes()
                .iter()
                .find(|node| node.kind() == crate::XmlNodeKind::Text)
                .unwrap();
            assert_eq!(node.value(&source), Some("ORIGINAL"));
            assert_eq!(node.value_with_source(view.raw_source()), Some("ORIGINAL"));
        }

        let mut reuse_count = 0;
        for _ in 0..4096 {
            let foreign = String::from(FOREIGN);
            if foreign.as_ptr() == original_pointer {
                reuse_count += 1;
                assert_eq!(node.value(&foreign), None);
                let foreign_view = crate::parse_document_view(&foreign).unwrap();
                assert_eq!(node.value_with_source(foreign_view.raw_source()), None);
            }
        }
        assert!(reuse_count > 0, "fixture allocator did not reuse storage");

        let compact = crate::parse_compact_document(ORIGINAL.to_owned()).unwrap();
        let compact_node = *compact
            .nodes()
            .iter()
            .find(|node| node.kind() == crate::XmlNodeKind::Text)
            .unwrap();
        assert_eq!(compact_node.value(compact.input()), Some("ORIGINAL"));
        assert_eq!(
            compact_node.value_with_source(compact.raw_source()),
            Some("ORIGINAL")
        );
        let cloned = compact.clone();
        assert_eq!(compact_node.value(cloned.input()), None);
        assert_eq!(
            compact_node.value_with_source(cloned.raw_source()),
            Some("ORIGINAL")
        );
    }

    #[test]
    fn raw_node_point_lookup_registration_follows_view_lifetime() {
        let source = String::from("<r>value</r>");
        let view = crate::parse_document_view(&source).unwrap();
        let text = view
            .node_ids()
            .find_map(|id| {
                view.node(id)
                    .filter(|node| node.kind() == crate::XmlNodeKind::Text)
                    .copied()
            })
            .unwrap();
        let cloned_view = view.clone();

        assert_eq!(text.value(&source), Some("value"));
        drop(view);
        assert_eq!(text.value(&source), Some("value"));
        drop(cloned_view);
        assert_eq!(text.value(&source), None);
    }

    #[test]
    fn raw_source_registration_overflow_preserves_parse_identity() {
        let sources = (0..300)
            .map(|index| format!("<r>value-{index}</r>"))
            .collect::<Vec<_>>();
        let views = sources
            .iter()
            .map(|source| crate::parse_document_view(source).unwrap())
            .collect::<Vec<_>>();

        for (index, (source, view)) in sources.iter().zip(&views).enumerate() {
            let text = view
                .nodes()
                .iter()
                .find(|node| node.kind() == crate::XmlNodeKind::Text)
                .unwrap();
            assert_eq!(text.value(source), Some(format!("value-{index}").as_str()));
        }
    }

    #[test]
    fn raw_source_owner_blocks_remain_unique_across_threads() {
        let threads: Vec<_> = (0..4)
            .map(|thread| {
                std::thread::spawn(move || {
                    (0..300)
                        .map(|index| {
                            let value = format!("value-{thread}-{index}");
                            let document =
                                crate::parse_compact_document(format!("<r>{value}</r>")).unwrap();
                            let text = *document
                                .nodes()
                                .iter()
                                .find(|node| node.kind() == crate::XmlNodeKind::Text)
                                .unwrap();
                            (document, text, value)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let documents: Vec<_> = threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect();

        for (index, (document, text, value)) in documents.iter().enumerate() {
            assert_eq!(text.value(document.input()), Some(value.as_str()));
            let foreign = &documents[(index + 1) % documents.len()].0;
            assert_eq!(text.value(foreign.input()), None);
            assert_eq!(text.value_with_source(foreign.raw_source()), None);
        }
    }
}
