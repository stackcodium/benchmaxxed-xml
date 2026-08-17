use std::{
    borrow::Cow,
    cell::{Cell, OnceCell, RefCell},
    collections::{HashMap, HashSet},
    error::Error,
    fmt, fs,
    hash::{Hash, Hasher},
    io::{Read, Write},
    ops::{Deref, Range},
    path::Path,
    rc::{Rc, Weak},
    str::FromStr,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::xpath_engine::{
    simple_descendant_filter, SimpleDescendantFilter, XPathArena, XPathArenaNodeKind,
    XPathArenaSelection, XPathArenaValue,
};

use crate::{
    parse_compact_document, parse_compact_document_bytes,
    parse_compact_document_bytes_tolerant_with_config, parse_compact_document_bytes_with_config,
    parse_compact_document_tolerant_with_config, parse_compact_document_with_config, ParserConfig,
    ToXmlValue, XPathContext, XPathError, XPathExpression, XPathVariables, XmlCompactDocument,
    XmlElement, XmlError, XmlLoadError, XmlMutationError, XmlNode, XmlNodeKind, XmlNodeRef,
    XmlParseOutcome, XmlPath, XmlSerializeOptions, XmlTreeStats, XmlWriteError,
    XMLNS_NAMESPACE_URI, XML_NAMESPACE_URI,
};

static NEXT_XML_DOM_DOCUMENT_ID: AtomicU64 = AtomicU64::new(1);
const XML_DOM_DOCUMENT_ID_BLOCK_SIZE: u64 = 256;

thread_local! {
    static XML_DOM_DOCUMENT_IDS: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
}

fn next_document_id() -> u64 {
    XML_DOM_DOCUMENT_IDS.with(|ids| {
        let (next, end) = ids.get();
        if next < end {
            ids.set((next + 1, end));
            return next;
        }

        let start = NEXT_XML_DOM_DOCUMENT_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                (next < u64::MAX).then(|| next.saturating_add(XML_DOM_DOCUMENT_ID_BLOCK_SIZE))
            })
            .expect("XML DOM document identity space exhausted");
        let end = start.saturating_add(XML_DOM_DOCUMENT_ID_BLOCK_SIZE);
        ids.set((start + 1, end));
        start
    })
}

/// An opaque logical identity that remains stable while its node remains in one document.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct XmlDomNodeId {
    document: u64,
    local: u64,
}

impl XmlDomNodeId {
    /// Returns document.
    pub fn document(self) -> u64 {
        self.document
    }

    /// Returns local.
    pub fn local(self) -> u64 {
        self.local
    }
}

/// One pugi-like document value backed by compact parsed records plus sparse mutable overlays.
///
/// [`Clone`] creates an independent document. Use [`Self::share`] when an explicit cheap mutable
/// alias is intended.
#[derive(Debug)]
pub struct XmlDom {
    inner: Rc<RefCell<XmlDomInner>>,
}

/// A uniquely held, thread-movable `XmlDom` state.
///
/// Convert this carrier back with [`Self::into_local`] after moving it to the destination thread.
/// It intentionally exposes no concurrent access and therefore adds no locking cost to [`XmlDom`].
#[derive(Debug)]
pub struct XmlDomSend {
    inner: XmlDomInner,
}

#[derive(Debug)]
struct XmlDomInner {
    state: XmlDomState,
    document_id: u64,
    generation: u64,
    structure_epoch: u64,
    next_node_id: u64,
}

#[derive(Clone, Debug, Default)]
struct IdentityCache {
    by_id: HashMap<u64, XmlPath>,
    by_path: HashMap<XmlPath, u64>,
}

#[derive(Clone, Debug)]
enum XmlDomState {
    Compact(XmlCompactDocument),
    Overlay {
        compact: XmlCompactDocument,
        edits: Box<SparseOverlay>,
    },
    Transition,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SparseOverlay {
    pub(crate) appended: HashMap<XmlPath, Vec<XmlNode>>,
    /// Complete logical child sequences for compact elements that received a non-append edit.
    /// Compact entries retain their zero-copy source records; only inserted/copied nodes are
    /// materialized. Paths in the map are current logical paths and are rebased after index changes.
    pub(crate) child_orders: HashMap<XmlPath, Vec<SparseChild>>,
    pub(crate) attributes: HashMap<(XmlPath, String), String>,
    pub(crate) added_attribute_order: HashMap<XmlPath, Vec<String>>,
    pub(crate) attribute_orders: HashMap<XmlPath, Vec<SparseAttribute>>,
    pub(crate) names: HashMap<XmlPath, String>,
    pub(crate) values: HashMap<XmlPath, String>,
    pub(crate) declaration: Option<Option<crate::XmlProcessingInstruction>>,
    pub(crate) doctype: Option<Option<crate::XmlDoctype>>,
    pub(crate) doctype_before_misc_index: Option<Option<usize>>,
    pub(crate) misc_before_root: Option<Vec<XmlNode>>,
    pub(crate) misc_after_root: Option<Vec<XmlNode>>,
    pub(crate) relocations: Vec<SparseRelocation>,
    pub(crate) mutations: usize,
    identity_cache: IdentityCache,
}

#[derive(Clone, Debug)]
pub(crate) enum SparseChild {
    Compact(crate::XmlViewNodeId),
    CompactCopy {
        id: crate::XmlViewNodeId,
        identity: u64,
    },
    Materialized(XmlNode),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum SparseCompactIdentity {
    Original(usize),
    Copy(u64),
}

#[derive(Clone, Debug)]
pub(crate) enum SparseAttribute {
    Compact(usize),
    Materialized(crate::XmlAttribute),
}

#[derive(Clone, Debug)]
pub(crate) struct SparseRelocation {
    pub(crate) parent: XmlPath,
    pub(crate) source_index: usize,
    pub(crate) destination_index: usize,
}

/// A generation-checked handle into an [`XmlDom`].
#[derive(Clone, Debug)]
pub struct XmlDomNode {
    inner: Rc<RefCell<XmlDomInner>>,
    path: RefCell<XmlDomPath>,
    id: Cell<XmlDomNodeId>,
    generation: Cell<u64>,
    structure_epoch: Cell<u64>,
}

/// A lazy document-order traversal over an [`XmlDomNode`] subtree.
///
/// Compact-backed documents create each stable handle only when it is yielded. This preserves the
/// mutation-aware handle contract without allocating handles for nodes a caller skips or drops.
#[derive(Debug)]
pub struct XmlDomWalk {
    storage: XmlDomWalkStorage,
}

/// A transient read-only node supplied by [`XmlDomNode::scan`].
///
/// Names and already-normalized values borrow the compact source. Values that require entity or
/// newline decoding are materialized only for the duration of the visitor call.
pub struct XmlDomScanNode<'a> {
    source: XmlDomScanNodeSource<'a>,
}

enum XmlDomScanNodeSource<'a> {
    Compact {
        document: &'a XmlCompactDocument,
        id: crate::XmlViewNodeId,
    },
    Owned {
        kind: XmlNodeKind,
        name: Option<String>,
        value: Option<String>,
        attributes: Vec<(String, String)>,
    },
}

/// One read-only attribute supplied by [`XmlDomScanNode::attributes`].
pub struct XmlDomScanAttribute<'a> {
    name: &'a str,
    value: Cow<'a, str>,
}

impl<'a> XmlDomScanAttribute<'a> {
    /// Returns the qualified attribute name.
    pub fn name(&self) -> &str {
        self.name
    }

    /// Returns the decoded attribute value, borrowing it when no normalization is required.
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// Iterator over attributes exposed by [`XmlDomScanNode::attributes`].
pub struct XmlDomScanAttributes<'a> {
    source: XmlDomScanAttributesSource<'a>,
}

enum XmlDomScanAttributesSource<'a> {
    Compact {
        document: &'a XmlCompactDocument,
        range: Range<usize>,
    },
    Owned(std::slice::Iter<'a, (String, String)>),
    Empty,
}

impl<'a> XmlDomScanNode<'a> {
    /// Returns this node's semantic kind.
    pub fn kind(&self) -> XmlNodeKind {
        match &self.source {
            XmlDomScanNodeSource::Compact { document, id } => document
                .node(*id)
                .expect("scan node references a compact record")
                .kind(),
            XmlDomScanNodeSource::Owned { kind, .. } => *kind,
        }
    }

    /// Returns the element or processing-instruction name.
    pub fn name(&self) -> Option<&str> {
        match &self.source {
            XmlDomScanNodeSource::Compact { document, id } => document.node_name(*id),
            XmlDomScanNodeSource::Owned { name, .. } => name.as_deref(),
        }
    }

    /// Returns the decoded scalar value for non-element nodes.
    pub fn value(&self) -> Result<Option<Cow<'_, str>>, XmlDomError> {
        match &self.source {
            XmlDomScanNodeSource::Compact { document, id } => {
                let kind = document
                    .node(*id)
                    .expect("scan node references a compact record")
                    .kind();
                let Some(value) = document.node_value(*id) else {
                    return Ok(None);
                };
                if document.compact_lexemes_are_borrowable {
                    return Ok(Some(Cow::Borrowed(value)));
                }
                crate::parser::decode_compact_lexeme_cow(
                    value,
                    if kind == XmlNodeKind::Text {
                        crate::parser::CompactLexemeKind::Text
                    } else {
                        crate::parser::CompactLexemeKind::Opaque
                    },
                    document.xml11,
                    document.config.attribute_whitespace,
                )
                .map(Some)
                .map_err(XmlDomError::from)
            }
            XmlDomScanNodeSource::Owned { value, .. } => Ok(value.as_deref().map(Cow::Borrowed)),
        }
    }

    /// Iterates decoded attributes without allocating for already-normalized compact values.
    pub fn attributes(&self) -> XmlDomScanAttributes<'_> {
        let source = match &self.source {
            XmlDomScanNodeSource::Compact { document, id } => {
                let node = document
                    .node(*id)
                    .expect("scan node references a compact record");
                if node.kind() == XmlNodeKind::Element {
                    XmlDomScanAttributesSource::Compact {
                        document,
                        range: node.attribute_range(),
                    }
                } else {
                    XmlDomScanAttributesSource::Empty
                }
            }
            XmlDomScanNodeSource::Owned { attributes, .. } => {
                XmlDomScanAttributesSource::Owned(attributes.iter())
            }
        };
        XmlDomScanAttributes { source }
    }
}

impl<'a> Iterator for XmlDomScanAttributes<'a> {
    type Item = Result<XmlDomScanAttribute<'a>, XmlDomError>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.source {
            XmlDomScanAttributesSource::Compact { document, range } => {
                let index = range.next()?;
                let name = document
                    .attribute_name(index)
                    .expect("scan attribute has a valid name range");
                let value = document
                    .attribute_value(index)
                    .expect("scan attribute has a valid value range");
                if document.compact_attribute_lexemes_are_borrowable {
                    return Some(Ok(XmlDomScanAttribute {
                        name,
                        value: Cow::Borrowed(value),
                    }));
                }
                Some(
                    crate::parser::decode_compact_lexeme_cow(
                        value,
                        crate::parser::CompactLexemeKind::Attribute,
                        document.xml11,
                        document.config.attribute_whitespace,
                    )
                    .map(|value| XmlDomScanAttribute { name, value })
                    .map_err(XmlDomError::from),
                )
            }
            XmlDomScanAttributesSource::Owned(attributes) => attributes.next().map(|item| {
                Ok(XmlDomScanAttribute {
                    name: &item.0,
                    value: Cow::Borrowed(&item.1),
                })
            }),
            XmlDomScanAttributesSource::Empty => None,
        }
    }
}

#[derive(Debug)]
enum XmlDomWalkStorage {
    Compact {
        inner: Rc<RefCell<XmlDomInner>>,
        topology: Rc<CompactQueryTopology>,
        document_id: u64,
        generation: u64,
        structure_epoch: u64,
        front: usize,
        back: usize,
    },
    CompactSelected {
        inner: Rc<RefCell<XmlDomInner>>,
        topology: Rc<CompactQueryTopology>,
        document_id: u64,
        generation: u64,
        structure_epoch: u64,
        selected: std::vec::IntoIter<u32>,
    },
    One(Option<XmlDomNode>),
    Materialized(std::vec::IntoIter<XmlDomNode>),
}

impl PartialEq for XmlDomNode {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl Eq for XmlDomNode {}

impl Hash for XmlDomNode {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id().hash(state);
    }
}

/// A document-ordered XPath element result.
///
/// Compact queries retain dense node identifiers and materialize individual [`XmlDomNode`]
/// handles only when slice-style access or borrowed iteration requires them. Use [`Self::len`]
/// and [`Self::is_empty`] without paying for one full handle per match, or [`Self::into_vec`] when
/// a collected `Vec` is specifically required.
#[derive(Debug)]
pub struct XmlDomNodeSet {
    inner: Rc<RefCell<XmlDomInner>>,
    generation: u64,
    structure_epoch: u64,
    storage: XmlDomNodeSetStorage,
    materialized: OnceCell<Vec<XmlDomNode>>,
}

#[derive(Clone, Debug)]
enum XmlDomNodeSetStorage {
    Compact {
        topology: Rc<CompactQueryTopology>,
        selected: Vec<u32>,
    },
    Paths(Vec<(XmlPath, u64)>),
}

#[derive(Clone, Copy, Debug)]
struct CompactTopologyEntry {
    node_id: u32,
    parent: u32,
    child_index: u32,
}

#[derive(Debug)]
struct CompactQueryTopology {
    inner: Weak<RefCell<XmlDomInner>>,
    include_all_nodes: bool,
    entries: OnceCell<Vec<CompactTopologyEntry>>,
}

#[derive(Clone, Debug)]
struct CompactQueryLocation {
    topology: Rc<CompactQueryTopology>,
    node_id: crate::XmlViewNodeId,
}

#[derive(Debug)]
enum XmlDomPath {
    Materialized(XmlPath),
    Compact {
        location: CompactQueryLocation,
        materialized: OnceCell<Box<XmlPath>>,
    },
}

impl XmlDomPath {
    fn compact(location: CompactQueryLocation) -> Self {
        Self::Compact {
            location,
            materialized: OnceCell::new(),
        }
    }

    fn compact_id(&self) -> Option<crate::XmlViewNodeId> {
        match self {
            Self::Materialized(_) => None,
            Self::Compact { location, .. } => Some(location.node_id),
        }
    }

    fn to_path(&self) -> XmlPath {
        self.deref().clone()
    }
}

impl Clone for XmlDomPath {
    fn clone(&self) -> Self {
        match self {
            Self::Materialized(path) => Self::Materialized(path.clone()),
            Self::Compact { location, .. } => Self::compact(location.clone()),
        }
    }
}

impl From<XmlPath> for XmlDomPath {
    fn from(path: XmlPath) -> Self {
        Self::Materialized(path)
    }
}

impl Deref for XmlDomPath {
    type Target = XmlPath;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Materialized(path) => path,
            Self::Compact {
                location,
                materialized,
            } => materialized
                .get_or_init(|| Box::new(location.materialize()))
                .as_ref(),
        }
    }
}

impl PartialEq for XmlDomPath {
    fn eq(&self, other: &Self) -> bool {
        self.deref() == other.deref()
    }
}

impl Eq for XmlDomPath {}

impl CompactQueryLocation {
    fn materialize(&self) -> XmlPath {
        let entries = self.topology.entries();
        let mut indexes = Vec::new();
        let node_id = u32::try_from(self.node_id.index()).expect("compact node id fits u32");
        let mut link = entries
            .binary_search_by_key(&node_id, |entry| entry.node_id)
            .expect("compact query node remains in its shared topology");
        loop {
            let entry = entries[link];
            if entry.parent == u32::MAX {
                break;
            }
            indexes.push(entry.child_index as usize);
            link = entry.parent as usize;
        }
        indexes.reverse();
        XmlPath::from_indexes(indexes)
    }
}

impl CompactQueryTopology {
    fn new(inner: &Rc<RefCell<XmlDomInner>>) -> Self {
        Self {
            inner: Rc::downgrade(inner),
            include_all_nodes: false,
            entries: OnceCell::new(),
        }
    }

    fn new_for_walk(inner: &Rc<RefCell<XmlDomInner>>) -> Self {
        Self {
            inner: Rc::downgrade(inner),
            include_all_nodes: true,
            entries: OnceCell::new(),
        }
    }

    fn entries(&self) -> &[CompactTopologyEntry] {
        self.entries.get_or_init(|| {
            let inner = self
                .inner
                .upgrade()
                .expect("query handles retain the XML document");
            let inner = inner.borrow();
            let compact = match &inner.state {
                XmlDomState::Compact(compact) | XmlDomState::Overlay { compact, .. } => compact,
                XmlDomState::Transition => unreachable!(),
            };
            compact_topology_entries(compact, self.include_all_nodes)
        })
    }
}

impl Iterator for XmlDomWalk {
    type Item = XmlDomNode;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.storage {
            XmlDomWalkStorage::Compact {
                inner,
                topology,
                document_id,
                generation,
                structure_epoch,
                front,
                back,
            } => {
                if *front >= *back {
                    return None;
                }
                let node_id = *front;
                *front += 1;
                Some(compact_walk_handle(
                    inner,
                    topology,
                    *document_id,
                    *generation,
                    *structure_epoch,
                    node_id,
                ))
            }
            XmlDomWalkStorage::Materialized(nodes) => nodes.next(),
            XmlDomWalkStorage::CompactSelected {
                inner,
                topology,
                document_id,
                generation,
                structure_epoch,
                selected,
            } => selected.next().map(|node_id| {
                compact_walk_handle(
                    inner,
                    topology,
                    *document_id,
                    *generation,
                    *structure_epoch,
                    node_id as usize,
                )
            }),
            XmlDomWalkStorage::One(node) => node.take(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.len();
        (remaining, Some(remaining))
    }
}

impl DoubleEndedIterator for XmlDomWalk {
    fn next_back(&mut self) -> Option<Self::Item> {
        match &mut self.storage {
            XmlDomWalkStorage::Compact {
                inner,
                topology,
                document_id,
                generation,
                structure_epoch,
                front,
                back,
            } => {
                if *front >= *back {
                    return None;
                }
                *back -= 1;
                Some(compact_walk_handle(
                    inner,
                    topology,
                    *document_id,
                    *generation,
                    *structure_epoch,
                    *back,
                ))
            }
            XmlDomWalkStorage::Materialized(nodes) => nodes.next_back(),
            XmlDomWalkStorage::CompactSelected {
                inner,
                topology,
                document_id,
                generation,
                structure_epoch,
                selected,
            } => selected.next_back().map(|node_id| {
                compact_walk_handle(
                    inner,
                    topology,
                    *document_id,
                    *generation,
                    *structure_epoch,
                    node_id as usize,
                )
            }),
            XmlDomWalkStorage::One(node) => node.take(),
        }
    }
}

impl ExactSizeIterator for XmlDomWalk {
    fn len(&self) -> usize {
        match &self.storage {
            XmlDomWalkStorage::Compact { front, back, .. } => back - front,
            XmlDomWalkStorage::Materialized(nodes) => nodes.len(),
            XmlDomWalkStorage::CompactSelected { selected, .. } => selected.len(),
            XmlDomWalkStorage::One(node) => usize::from(node.is_some()),
        }
    }
}

impl std::iter::FusedIterator for XmlDomWalk {}

fn compact_walk_handle(
    inner: &Rc<RefCell<XmlDomInner>>,
    topology: &Rc<CompactQueryTopology>,
    document_id: u64,
    generation: u64,
    structure_epoch: u64,
    node_id: usize,
) -> XmlDomNode {
    XmlDomNode {
        inner: Rc::clone(inner),
        path: RefCell::new(XmlDomPath::compact(CompactQueryLocation {
            topology: Rc::clone(topology),
            node_id: crate::XmlViewNodeId(node_id),
        })),
        id: Cell::new(XmlDomNodeId {
            document: document_id,
            local: node_id as u64,
        }),
        generation: Cell::new(generation),
        structure_epoch: Cell::new(structure_epoch),
    }
}

fn new_node_handle(
    inner: &Rc<RefCell<XmlDomInner>>,
    path: XmlPath,
    generation: Option<u64>,
) -> Option<XmlDomNode> {
    let (id, generation, structure_epoch) = if let Ok(mut document) = inner.try_borrow_mut() {
        let local = node_local_id_at_path(&mut document, &path)?;
        let id = XmlDomNodeId {
            document: document.document_id,
            local,
        };
        let generation = generation.unwrap_or(document.generation);
        (id, generation, document.structure_epoch)
    } else {
        // XPath arena conversion can create handles while the document is immutably borrowed.
        // Defer identity registration until the handle is first used, as sibling handles do.
        let document = inner.borrow();
        let id = XmlDomNodeId {
            document: document.document_id,
            local: u64::MAX,
        };
        let generation = generation.unwrap_or(document.generation);
        (id, generation, document.structure_epoch)
    };
    Some(XmlDomNode {
        inner: Rc::clone(inner),
        path: RefCell::new(path.into()),
        id: Cell::new(id),
        generation: Cell::new(generation),
        structure_epoch: Cell::new(structure_epoch),
    })
}

impl XmlDomNodeSet {
    fn from_compact(
        inner: &Rc<RefCell<XmlDomInner>>,
        generation: u64,
        topology: Rc<CompactQueryTopology>,
        selected: Vec<u32>,
    ) -> Self {
        let structure_epoch = inner.borrow().structure_epoch;
        Self {
            inner: Rc::clone(inner),
            generation,
            structure_epoch,
            storage: XmlDomNodeSetStorage::Compact { topology, selected },
            materialized: OnceCell::new(),
        }
    }

    fn from_paths(inner: &Rc<RefCell<XmlDomInner>>, generation: u64, paths: Vec<XmlPath>) -> Self {
        let (structure_epoch, paths) = {
            let mut document = inner.borrow_mut();
            let structure_epoch = document.structure_epoch;
            let paths = paths
                .into_iter()
                .map(|path| {
                    let id = node_local_id_at_path(&mut document, &path)
                        .expect("XPath result path resolves to a logical node");
                    (path, id)
                })
                .collect();
            (structure_epoch, paths)
        };
        Self {
            inner: Rc::clone(inner),
            generation,
            structure_epoch,
            storage: XmlDomNodeSetStorage::Paths(paths),
            materialized: OnceCell::new(),
        }
    }

    /// Returns the number of selected elements without materializing per-node handles.
    pub fn len(&self) -> usize {
        match &self.storage {
            XmlDomNodeSetStorage::Compact { selected, .. } => selected.len(),
            XmlDomNodeSetStorage::Paths(paths) => paths.len(),
        }
    }

    /// Returns whether the selection contains no elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Materializes the selection as independent handles.
    pub fn into_vec(self) -> Vec<XmlDomNode> {
        let Self {
            inner,
            generation,
            structure_epoch,
            storage,
            materialized,
        } = self;
        materialized.into_inner().unwrap_or_else(|| {
            Self::materialize_storage(&inner, generation, structure_epoch, &storage)
        })
    }

    fn materialize_storage(
        inner: &Rc<RefCell<XmlDomInner>>,
        generation: u64,
        structure_epoch: u64,
        storage: &XmlDomNodeSetStorage,
    ) -> Vec<XmlDomNode> {
        let document_id = inner.borrow().document_id;
        match storage {
            XmlDomNodeSetStorage::Compact { topology, selected } => selected
                .iter()
                .map(|&node_id| XmlDomNode {
                    inner: Rc::clone(inner),
                    path: RefCell::new(XmlDomPath::compact(CompactQueryLocation {
                        topology: Rc::clone(topology),
                        node_id: crate::XmlViewNodeId(node_id as usize),
                    })),
                    id: Cell::new(XmlDomNodeId {
                        document: document_id,
                        local: u64::from(node_id),
                    }),
                    generation: Cell::new(generation),
                    structure_epoch: Cell::new(structure_epoch),
                })
                .collect(),
            XmlDomNodeSetStorage::Paths(paths) => paths
                .iter()
                .map(|(path, local)| XmlDomNode {
                    inner: Rc::clone(inner),
                    path: RefCell::new(path.clone().into()),
                    id: Cell::new(XmlDomNodeId {
                        document: document_id,
                        local: *local,
                    }),
                    generation: Cell::new(generation),
                    structure_epoch: Cell::new(structure_epoch),
                })
                .collect(),
        }
    }
}

impl Clone for XmlDomNodeSet {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
            generation: self.generation,
            structure_epoch: self.structure_epoch,
            storage: self.storage.clone(),
            materialized: OnceCell::new(),
        }
    }
}

impl Deref for XmlDomNodeSet {
    type Target = [XmlDomNode];

    fn deref(&self) -> &Self::Target {
        self.materialized
            .get_or_init(|| {
                Self::materialize_storage(
                    &self.inner,
                    self.generation,
                    self.structure_epoch,
                    &self.storage,
                )
            })
            .as_slice()
    }
}

impl IntoIterator for XmlDomNodeSet {
    type Item = XmlDomNode;
    type IntoIter = std::vec::IntoIter<XmlDomNode>;

    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

impl<'a> IntoIterator for &'a XmlDomNodeSet {
    type Item = &'a XmlDomNode;
    type IntoIter = std::slice::Iter<'a, XmlDomNode>;

    fn into_iter(self) -> Self::IntoIter {
        self.deref().iter()
    }
}

impl From<XmlDomNodeSet> for Vec<XmlDomNode> {
    fn from(nodes: XmlDomNodeSet) -> Self {
        nodes.into_vec()
    }
}

/// A streaming builder for generated documents that finishes as the compact `XmlDom` state.
///
/// Attributes should be added before child content. Invalid names, characters, or call order are
/// reported when [`XmlDom::build`] or [`XmlDom::build_with_capacity`] finishes.
pub struct XmlDomElementBuilder<'a, 'name> {
    source: &'a mut String,
    error: &'a mut Option<XmlError>,
    name: &'name str,
    start_open: bool,
}

impl XmlDomElementBuilder<'_, '_> {
    /// Reserves additional bytes in the generated compact source.
    pub fn reserve(&mut self, additional: usize) -> &mut Self {
        self.source.reserve(additional);
        self
    }

    /// Returns attribute.
    pub fn attribute(&mut self, name: impl AsRef<str>, value: impl AsRef<str>) -> &mut Self {
        if !self.start_open {
            self.record_order_error();
            return self;
        }
        self.source.push(' ');
        self.source.push_str(name.as_ref());
        self.source.push_str("=\"");
        push_builder_attribute(self.source, value.as_ref());
        self.source.push('"');
        self
    }

    /// Returns attribute typed.
    pub fn attribute_typed<T: ToXmlValue>(&mut self, name: impl AsRef<str>, value: T) -> &mut Self {
        self.attribute(name, value.to_xml_value())
    }

    /// Appends a display-formatted attribute without allocating an intermediate value string.
    pub fn attribute_display<T: fmt::Display>(
        &mut self,
        name: impl AsRef<str>,
        value: T,
    ) -> &mut Self {
        if !self.start_open {
            self.record_order_error();
            return self;
        }
        self.source.push(' ');
        self.source.push_str(name.as_ref());
        self.source.push_str("=\"");
        let mut output = BuilderAttributeOutput(self.source);
        if fmt::Write::write_fmt(&mut output, format_args!("{value}")).is_err() {
            self.record_format_error();
        }
        self.source.push('"');
        self
    }

    /// Returns text.
    pub fn text(&mut self, value: impl AsRef<str>) -> &mut Self {
        self.close_start();
        push_builder_text(self.source, value.as_ref());
        self
    }

    /// Appends display-formatted text without allocating an intermediate value string.
    pub fn text_display<T: fmt::Display>(&mut self, value: T) -> &mut Self {
        self.close_start();
        let mut output = BuilderTextOutput(self.source);
        if fmt::Write::write_fmt(&mut output, format_args!("{value}")).is_err() {
            self.record_format_error();
        }
        self
    }

    /// Returns element.
    pub fn element<N, F>(&mut self, name: N, build: F) -> &mut Self
    where
        N: AsRef<str>,
        F: FnOnce(&mut XmlDomElementBuilder<'_, '_>),
    {
        self.close_start();
        let name = name.as_ref();
        self.source.push('<');
        self.source.push_str(name);
        let mut child = XmlDomElementBuilder {
            source: self.source,
            error: self.error,
            name,
            start_open: true,
        };
        build(&mut child);
        child.finish();
        self
    }

    /// Returns comment.
    pub fn comment(&mut self, value: impl AsRef<str>) -> &mut Self {
        self.close_start();
        self.source.push_str("<!--");
        self.source.push_str(value.as_ref());
        self.source.push_str("-->");
        self
    }

    /// Returns cdata.
    pub fn cdata(&mut self, value: impl AsRef<str>) -> &mut Self {
        self.close_start();
        self.source.push_str("<![CDATA[");
        let mut remaining = value.as_ref();
        while let Some(index) = remaining.find("]]>") {
            self.source.push_str(&remaining[..index]);
            self.source.push_str("]]]]><![CDATA[>");
            remaining = &remaining[index + 3..];
        }
        self.source.push_str(remaining);
        self.source.push_str("]]>");
        self
    }

    /// Returns processing instruction.
    pub fn processing_instruction(
        &mut self,
        target: impl AsRef<str>,
        data: impl AsRef<str>,
    ) -> &mut Self {
        self.close_start();
        self.source.push_str("<?");
        self.source.push_str(target.as_ref());
        if !data.as_ref().is_empty() {
            self.source.push(' ');
            self.source.push_str(data.as_ref());
        }
        self.source.push_str("?>");
        self
    }

    fn close_start(&mut self) {
        if self.start_open {
            self.source.push('>');
            self.start_open = false;
        }
    }

    fn finish(&mut self) {
        if self.start_open {
            self.source.push_str("/>");
            self.start_open = false;
        } else {
            self.source.push_str("</");
            self.source.push_str(self.name);
            self.source.push('>');
        }
    }

    fn record_order_error(&mut self) {
        if self.error.is_none() {
            *self.error = Some(XmlError::new(
                crate::XmlErrorKind::InvalidDocumentStructure,
                self.source.len(),
            ));
        }
    }

    fn record_format_error(&mut self) {
        if self.error.is_none() {
            *self.error = Some(XmlError::new(
                crate::XmlErrorKind::InvalidDocumentStructure,
                self.source.len(),
            ));
        }
    }
}

struct BuilderTextOutput<'a>(&'a mut String);

impl fmt::Write for BuilderTextOutput<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        push_builder_text(self.0, value);
        Ok(())
    }
}

struct BuilderAttributeOutput<'a>(&'a mut String);

impl fmt::Write for BuilderAttributeOutput<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        push_builder_attribute(self.0, value);
        Ok(())
    }
}

fn push_builder_text(output: &mut String, value: &str) {
    if value
        .bytes()
        .all(|byte| (0x20..=0x7e).contains(&byte) && !matches!(byte, b'&' | b'<' | b'>'))
    {
        output.push_str(value);
        return;
    }
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(character),
        }
    }
}

fn push_builder_attribute(output: &mut String, value: &str) {
    if value.bytes().all(|byte| {
        (0x20..=0x7e).contains(&byte) && !matches!(byte, b'&' | b'<' | b'"' | b'\t' | b'\n' | b'\r')
    }) {
        output.push_str(value);
        return;
    }
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
}

/// A node-set member returned by XPath over [`XmlDom`].
#[derive(Clone, Debug)]
pub enum XmlDomXPathNode {
    /// Indicates `Element`.
    Element(XmlDomNode),
    /// Indicates `Attribute`.
    Attribute {
        /// The owner.
        owner: XmlDomNode,
        /// The name.
        name: String,
    },
    /// Indicates `Text`.
    Text(XmlDomNode),
    /// Indicates `Comment`.
    Comment(XmlDomNode),
    /// Indicates `ProcessingInstruction`.
    ProcessingInstruction(XmlDomNode),
    /// Indicates `Namespace`.
    Namespace {
        /// The owner.
        owner: XmlDomNode,
        /// The prefix.
        prefix: Option<String>,
        /// The uri.
        uri: String,
    },
}

impl XmlDomXPathNode {
    /// Returns this value as element when it has that kind.
    pub fn as_element(&self) -> Option<&XmlDomNode> {
        match self {
            Self::Element(node) => Some(node),
            _ => None,
        }
    }

    /// Returns the selected tree node. Attributes and namespace nodes instead expose an owner.
    pub fn as_node(&self) -> Option<&XmlDomNode> {
        match self {
            Self::Element(node)
            | Self::Text(node)
            | Self::Comment(node)
            | Self::ProcessingInstruction(node) => Some(node),
            Self::Attribute { .. } | Self::Namespace { .. } => None,
        }
    }

    /// Returns owner.
    pub fn owner(&self) -> Option<&XmlDomNode> {
        match self {
            Self::Attribute { owner, .. } | Self::Namespace { owner, .. } => Some(owner),
            _ => None,
        }
    }

    /// Returns this value as attribute when it has that kind.
    pub fn as_attribute(&self) -> Option<(&XmlDomNode, &str)> {
        match self {
            Self::Attribute { owner, name } => Some((owner, name)),
            _ => None,
        }
    }

    /// Returns this value as namespace when it has that kind.
    pub fn as_namespace(&self) -> Option<(&XmlDomNode, Option<&str>, &str)> {
        match self {
            Self::Namespace { owner, prefix, uri } => Some((owner, prefix.as_deref(), uri)),
            _ => None,
        }
    }

    /// Returns the XPath string-value of this result.
    pub fn string_value(&self) -> Result<String, XmlDomError> {
        match self {
            Self::Element(node) => Ok(node.select_string(".")?.unwrap_or_default()),
            Self::Attribute { owner, name } => Ok(owner.attribute(name)?.unwrap_or_default()),
            Self::Text(node) | Self::Comment(node) | Self::ProcessingInstruction(node) => {
                Ok(node.value()?.unwrap_or_default())
            }
            Self::Namespace { uri, .. } => Ok(uri.clone()),
        }
    }
}

/// The byte coordinate used by a retained node position.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum XmlSourceCoordinates {
    /// Bytes in the normalized UTF-8 source retained by the DOM.
    ///
    /// For `&str` input this is also the original input coordinate. Byte-oriented parsing may
    /// decode UTF-16, UTF-32, or Latin-1 first, so this coordinate can differ from the original
    /// encoded byte position used by parse errors.
    DecodedUtf8,
}

/// A source position for a node retained from parsed input.
///
/// `byte` is zero-based. `line` and `column` are one-based, with columns counted in Unicode scalar
/// values rather than bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct XmlSourcePosition {
    /// The byte.
    pub byte: usize,
    /// The line.
    pub line: usize,
    /// The column.
    pub column: usize,
    /// The coordinates.
    pub coordinates: XmlSourceCoordinates,
}

/// A failed mutable-DOM access, query, or edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XmlDomError {
    /// Parsing failed while constructing or replacing document state.
    Parse(XmlError),
    /// XPath compilation or evaluation failed.
    XPath(XPathError),
    /// A requested edit would produce invalid XML or targeted an invalid index.
    Mutation(crate::XmlMutationError),
    /// A lexical XML name could not be interpreted as a namespace-qualified name.
    Namespace(crate::XmlNamespaceError),
    /// The whole document was reset or replaced after this handle was created.
    StaleHandle,
    /// The node or one of its ancestors was removed or replaced.
    DeletedHandle,
    /// The operation requires an element but the handle refers to another node kind.
    NotElement,
    /// The operation attempted to position content as a sibling of the document element.
    RootHasNoSiblings,
    /// The requested target is incompatible with the operation or no longer resolves.
    InvalidTarget,
    /// An operation requiring one document received handles from different documents.
    WrongDocument,
}

/// Failure to resolve or serialize a selected DOM node.
#[derive(Debug)]
pub enum XmlDomOutputError {
    /// Indicates `Dom`.
    Dom(XmlDomError),
    /// Indicates `Write`.
    Write(XmlWriteError),
}

impl fmt::Display for XmlDomOutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dom(error) => error.fmt(formatter),
            Self::Write(error) => error.fmt(formatter),
        }
    }
}

impl Error for XmlDomOutputError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Dom(error) => Some(error),
            Self::Write(error) => Some(error),
        }
    }
}

impl From<XmlDomError> for XmlDomOutputError {
    fn from(error: XmlDomError) -> Self {
        Self::Dom(error)
    }
}

impl From<XmlWriteError> for XmlDomOutputError {
    fn from(error: XmlWriteError) -> Self {
        Self::Write(error)
    }
}

impl fmt::Display for XmlDomError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => error.fmt(formatter),
            Self::XPath(error) => error.fmt(formatter),
            Self::Mutation(error) => error.fmt(formatter),
            Self::Namespace(error) => error.fmt(formatter),
            Self::StaleHandle => formatter.write_str("XML node handle was invalidated by mutation"),
            Self::DeletedHandle => formatter.write_str("XML node handle refers to a deleted node"),
            Self::NotElement => formatter.write_str("XML operation requires an element node"),
            Self::RootHasNoSiblings => formatter.write_str("the document element has no siblings"),
            Self::InvalidTarget => formatter.write_str("XML node target no longer exists"),
            Self::WrongDocument => formatter.write_str("XML node belongs to another document"),
        }
    }
}

impl Error for XmlDomError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Parse(error) => Some(error),
            Self::XPath(error) => Some(error),
            Self::Mutation(error) => Some(error),
            Self::Namespace(error) => Some(error),
            _ => None,
        }
    }
}

impl From<XmlError> for XmlDomError {
    fn from(error: XmlError) -> Self {
        Self::Parse(error)
    }
}

impl From<XPathError> for XmlDomError {
    fn from(error: XPathError) -> Self {
        Self::XPath(error)
    }
}

impl From<crate::XmlMutationError> for XmlDomError {
    fn from(error: crate::XmlMutationError) -> Self {
        Self::Mutation(error)
    }
}

impl From<crate::XmlNamespaceError> for XmlDomError {
    fn from(error: crate::XmlNamespaceError) -> Self {
        Self::Namespace(error)
    }
}

#[inline]
fn validate_mutation_name(name: String) -> Result<String, XmlDomError> {
    if crate::syntax::is_valid_name(&name) {
        Ok(name)
    } else {
        Err(crate::XmlMutationError::InvalidName(name).into())
    }
}

impl Clone for XmlDom {
    fn clone(&self) -> Self {
        self.deep_clone()
    }
}

impl XmlDomSend {
    /// Restores the normal single-threaded facade on the current thread.
    pub fn into_local(self) -> XmlDom {
        XmlDom {
            inner: Rc::new(RefCell::new(self.inner)),
        }
    }
}

impl XmlDom {
    /// Creates an empty editable document with a validated document-element name.
    ///
    /// The document starts in the same compact representation as parsed input, so subsequent
    /// facade mutations preserve the compact backing where possible.
    pub fn new(root_name: impl Into<String>) -> Result<Self, XmlError> {
        XmlCompactDocument::empty_with_root(root_name.into()).map(Self::from_compact)
    }

    /// Builds a generated document directly into the compact representation.
    pub fn build<F>(root_name: impl Into<String>, build: F) -> Result<Self, XmlError>
    where
        F: FnOnce(&mut XmlDomElementBuilder<'_, '_>),
    {
        Self::build_with_capacity(root_name, 0, build)
    }

    /// Builds a generated compact document with source capacity reserved up front.
    pub fn build_with_capacity<F>(
        root_name: impl Into<String>,
        source_capacity: usize,
        build: F,
    ) -> Result<Self, XmlError>
    where
        F: FnOnce(&mut XmlDomElementBuilder<'_, '_>),
    {
        let root_name = root_name.into();
        let mut source = String::with_capacity(source_capacity.max(root_name.len() + 3));
        source.push('<');
        source.push_str(&root_name);
        let mut error = None;
        let mut root = XmlDomElementBuilder {
            source: &mut source,
            error: &mut error,
            name: &root_name,
            start_open: true,
        };
        build(&mut root);
        root.finish();
        if let Some(error) = error {
            return Err(error);
        }
        let mut compact = parse_compact_document(source)?;
        compact.default_serialization_is_source =
            crate::dom::DefaultSerializationSourceCache::known(true);
        Ok(Self::from_compact(compact))
    }

    /// Parses a fully preserved compact document behind the primary editable facade.
    pub fn parse(input: impl Into<String>) -> Result<Self, XmlError> {
        let compact = parse_compact_document(input.into())?;
        Ok(Self::from_compact(compact))
    }

    /// Parses with config.
    pub fn parse_with_config(
        input: impl Into<String>,
        config: ParserConfig,
    ) -> Result<Self, XmlError> {
        parse_compact_document_with_config(input.into(), config).map(Self::from_compact)
    }

    /// Parses a useful document prefix without changing the strict behavior of [`Self::parse`].
    pub fn parse_tolerant(input: impl Into<String>) -> Result<XmlParseOutcome<Self>, XmlError> {
        Self::parse_tolerant_with_config(input, ParserConfig::preserve_all())
    }

    /// Parses tolerant with config.
    pub fn parse_tolerant_with_config(
        input: impl Into<String>,
        config: ParserConfig,
    ) -> Result<XmlParseOutcome<Self>, XmlError> {
        parse_compact_document_tolerant_with_config(input.into(), config).map(|outcome| {
            XmlParseOutcome {
                value: Self::from_compact(outcome.value),
                diagnostic: outcome.diagnostic,
                consumed_bytes: outcome.consumed_bytes,
            }
        })
    }

    /// Returns from compact.
    pub fn from_compact(compact: XmlCompactDocument) -> Self {
        let next_node_id = compact.nodes.len() as u64;
        Self {
            inner: Rc::new(RefCell::new(XmlDomInner {
                state: XmlDomState::Compact(compact),
                document_id: next_document_id(),
                generation: 0,
                structure_epoch: 0,
                next_node_id,
            })),
        }
    }

    /// Creates an explicit cheap mutable alias of this document.
    ///
    /// Handles and aliases returned from either value observe the same mutations.
    pub fn share(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
        }
    }

    /// Creates an independent copy that preserves compact and sparse-overlay representation.
    pub fn deep_clone(&self) -> Self {
        let inner = self.inner.borrow();
        Self {
            inner: Rc::new(RefCell::new(XmlDomInner {
                state: inner.state.clone(),
                document_id: next_document_id(),
                generation: 0,
                structure_epoch: inner.structure_epoch,
                next_node_id: inner.next_node_id,
            })),
        }
    }

    /// Converts a uniquely held document into a thread-movable carrier.
    ///
    /// This fails and returns the original facade while a shared alias, node handle, or node set
    /// still retains the document.
    pub fn try_into_send(self) -> Result<XmlDomSend, Self> {
        let Self { inner } = self;
        match Rc::try_unwrap(inner) {
            Ok(inner) => Ok(XmlDomSend {
                inner: inner.into_inner(),
            }),
            Err(inner) => Err(Self { inner }),
        }
    }

    /// Resets this facade to one validated empty root and invalidates existing node handles.
    pub fn reset(&self, root_name: impl Into<String>) -> Result<(), XmlError> {
        let compact = XmlCompactDocument::empty_with_root(root_name.into())?;
        let mut inner = self.inner.borrow_mut();
        inner.document_id = next_document_id();
        inner.next_node_id = compact.nodes().len() as u64;
        inner.structure_epoch = 0;
        inner.state = XmlDomState::Compact(compact);
        bump_generation(&mut inner);
        Ok(())
    }

    /// Replaces this facade with an independent copy of another facade's optimized state.
    pub fn copy_from(&self, source: &Self) -> Result<(), XmlDomError> {
        if Rc::ptr_eq(&self.inner, &source.inner) {
            return Ok(());
        }
        let source_inner = source.inner.borrow();
        let state = source_inner.state.clone();
        let next_node_id = source_inner.next_node_id;
        drop(source_inner);
        let mut inner = self.inner.borrow_mut();
        inner.document_id = next_document_id();
        inner.next_node_id = next_node_id;
        inner.structure_epoch = 0;
        inner.state = state;
        bump_generation(&mut inner);
        Ok(())
    }

    /// Parses bytes.
    pub fn parse_bytes(input: &[u8]) -> Result<Self, XmlError> {
        parse_compact_document_bytes(input).map(Self::from_compact)
    }

    /// Parses bytes with config.
    pub fn parse_bytes_with_config(input: &[u8], config: ParserConfig) -> Result<Self, XmlError> {
        parse_compact_document_bytes_with_config(input, config).map(Self::from_compact)
    }

    /// Parses bytes tolerant.
    pub fn parse_bytes_tolerant(input: &[u8]) -> Result<XmlParseOutcome<Self>, XmlError> {
        Self::parse_bytes_tolerant_with_config(input, ParserConfig::preserve_all())
    }

    /// Parses bytes tolerant with config.
    pub fn parse_bytes_tolerant_with_config(
        input: &[u8],
        config: ParserConfig,
    ) -> Result<XmlParseOutcome<Self>, XmlError> {
        parse_compact_document_bytes_tolerant_with_config(input, config).map(|outcome| {
            XmlParseOutcome {
                value: Self::from_compact(outcome.value),
                diagnostic: outcome.diagnostic,
                consumed_bytes: outcome.consumed_bytes,
            }
        })
    }

    /// Reads the value.
    pub fn read<R: Read>(mut reader: R) -> Result<Self, XmlLoadError> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err(|source| XmlLoadError::Io { path: None, source })?;
        Self::parse_bytes(&bytes).map_err(XmlLoadError::Parse)
    }

    /// Reads with config.
    pub fn read_with_config<R: Read>(
        mut reader: R,
        config: ParserConfig,
    ) -> Result<Self, XmlLoadError> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err(|source| XmlLoadError::Io { path: None, source })?;
        Self::parse_bytes_with_config(&bytes, config).map_err(XmlLoadError::Parse)
    }

    /// Loads the value.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, XmlLoadError> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|source| XmlLoadError::Io {
            path: Some(path.to_owned()),
            source,
        })?;
        Self::parse_bytes(&bytes).map_err(XmlLoadError::Parse)
    }

    /// Loads with config.
    pub fn load_with_config(
        path: impl AsRef<Path>,
        config: ParserConfig,
    ) -> Result<Self, XmlLoadError> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|source| XmlLoadError::Io {
            path: Some(path.to_owned()),
            source,
        })?;
        Self::parse_bytes_with_config(&bytes, config).map_err(XmlLoadError::Parse)
    }

    /// Returns root.
    pub fn root(&self) -> XmlDomNode {
        self.handle(XmlPath::root())
    }

    /// Returns the document element and all element descendants in document order.
    ///
    /// Unedited compact documents select dense element identifiers before constructing stable
    /// handles. This avoids creating and then cloning a temporary root handle.
    pub fn walk_elements(&self) -> Result<XmlDomWalk, XmlDomError> {
        let compact_walk = {
            let inner = self.inner.borrow();
            match &inner.state {
                XmlDomState::Compact(document) => {
                    let root = document.root.index();
                    let single = document.stats.elements == 1;
                    let selected = (!single).then(|| {
                        let back = document
                            .node(document.root)
                            .expect("compact document root exists")
                            .next_subtree();
                        (root..back)
                            .filter(|&index| {
                                document
                                    .node(crate::XmlViewNodeId(index))
                                    .is_some_and(|node| node.kind() == XmlNodeKind::Element)
                            })
                            .map(|index| u32::try_from(index).expect("compact node id fits u32"))
                            .collect::<Vec<_>>()
                    });
                    Some((
                        inner.document_id,
                        inner.generation,
                        inner.structure_epoch,
                        root,
                        selected,
                    ))
                }
                XmlDomState::Overlay { .. } => None,
                XmlDomState::Transition => unreachable!(),
            }
        };

        if let Some((document_id, generation, structure_epoch, root, selected)) = compact_walk {
            if let Some(selected) = selected {
                return Ok(XmlDomWalk {
                    storage: XmlDomWalkStorage::CompactSelected {
                        inner: Rc::clone(&self.inner),
                        topology: Rc::new(CompactQueryTopology::new_for_walk(&self.inner)),
                        document_id,
                        generation,
                        structure_epoch,
                        selected: selected.into_iter(),
                    },
                });
            }
            return Ok(XmlDomWalk {
                storage: XmlDomWalkStorage::One(Some(XmlDomNode {
                    inner: Rc::clone(&self.inner),
                    path: RefCell::new(XmlPath::root().into()),
                    id: Cell::new(XmlDomNodeId {
                        document: document_id,
                        local: root as u64,
                    }),
                    generation: Cell::new(generation),
                    structure_epoch: Cell::new(structure_epoch),
                })),
            });
        }

        self.root().walk_elements()
    }

    /// Returns declaration.
    pub fn declaration(&self) -> Option<crate::XmlProcessingInstruction> {
        let inner = self.inner.borrow();
        match &inner.state {
            XmlDomState::Compact(compact) => compact.metadata.declaration.clone(),
            XmlDomState::Overlay { compact, edits } => edits
                .declaration
                .clone()
                .unwrap_or_else(|| compact.metadata.declaration.clone()),
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Sets declaration.
    pub fn set_declaration(&self, data: impl Into<String>) -> Result<(), XmlDomError> {
        let data = data.into();
        crate::mutation::validate_characters(&data)?;
        let candidate = format!("<?xml {data}?><root/>");
        crate::validate_document(&candidate)
            .map_err(|_| crate::XmlMutationError::InvalidDeclaration)?;
        let declaration = crate::XmlProcessingInstruction {
            target: "xml".to_owned(),
            data,
        };
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        match &mut inner.state {
            XmlDomState::Overlay { edits, .. } => edits.declaration = Some(Some(declaration)),
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        }
        Ok(())
    }

    /// Clears declaration.
    pub fn clear_declaration(&self) {
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        match &mut inner.state {
            XmlDomState::Overlay { edits, .. } => edits.declaration = Some(None),
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        }
    }

    /// Returns doctype.
    pub fn doctype(&self) -> Option<crate::XmlDoctype> {
        let inner = self.inner.borrow();
        match &inner.state {
            XmlDomState::Compact(compact) => compact.metadata.doctype.clone(),
            XmlDomState::Overlay { compact, edits } => edits
                .doctype
                .clone()
                .unwrap_or_else(|| compact.metadata.doctype.clone()),
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Sets doctype.
    pub fn set_doctype(&self, doctype: crate::XmlDoctype) -> Result<(), XmlDomError> {
        crate::mutation::validate_doctype(&doctype)?;
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        match &mut inner.state {
            XmlDomState::Overlay { compact, edits } => {
                let before_len = edits
                    .misc_before_root
                    .as_ref()
                    .map_or(compact.metadata.misc_before_root.len(), Vec::len);
                edits.doctype = Some(Some(doctype));
                edits.doctype_before_misc_index = Some(Some(before_len));
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        }
        Ok(())
    }

    /// Sets doctype name.
    pub fn set_doctype_name(&self, name: impl Into<String>) -> Result<(), XmlDomError> {
        self.set_doctype(crate::XmlDoctype {
            name: name.into(),
            public_id: None,
            system_id: None,
            internal_subset: None,
        })
    }

    /// Clears doctype.
    pub fn clear_doctype(&self) {
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        match &mut inner.state {
            XmlDomState::Overlay { edits, .. } => {
                edits.doctype = Some(None);
                edits.doctype_before_misc_index = Some(None);
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        }
    }

    /// Returns top-level comments and processing instructions before the document element.
    ///
    /// Values are cloned into an owned vector so callers can inspect them without holding a
    /// `RefCell` borrow and may safely mutate the document while iterating the result.
    pub fn before_root_nodes(&self) -> Vec<XmlNode> {
        document_misc_nodes(&self.inner.borrow(), false).to_vec()
    }

    /// Returns top-level comments and processing instructions after the document element.
    ///
    /// Values are cloned into an owned vector so callers can inspect them without holding a
    /// `RefCell` borrow and may safely mutate the document while iterating the result.
    pub fn after_root_nodes(&self) -> Vec<XmlNode> {
        document_misc_nodes(&self.inner.borrow(), true).to_vec()
    }

    /// Appends a comment or processing instruction before the document element.
    pub fn append_before_root(&self, node: XmlNode) -> Result<(), XmlDomError> {
        append_document_misc(&mut self.inner.borrow_mut(), node, false)
    }

    /// Appends a comment or processing instruction after the document element.
    pub fn append_after_root(&self, node: XmlNode) -> Result<(), XmlDomError> {
        append_document_misc(&mut self.inner.borrow_mut(), node, true)
    }

    /// Removes and returns a top-level node before the document element by ordered index.
    pub fn remove_before_root(&self, index: usize) -> Result<XmlNode, XmlDomError> {
        remove_document_misc(&mut self.inner.borrow_mut(), index, false)
    }

    /// Removes and returns a top-level node after the document element by ordered index.
    pub fn remove_after_root(&self, index: usize) -> Result<XmlNode, XmlDomError> {
        remove_document_misc(&mut self.inner.borrow_mut(), index, true)
    }

    /// Replaces and returns a top-level node before the document element by ordered index.
    pub fn replace_before_root(&self, index: usize, node: XmlNode) -> Result<XmlNode, XmlDomError> {
        replace_document_misc(&mut self.inner.borrow_mut(), index, node, false)
    }

    /// Replaces and returns a top-level node after the document element by ordered index.
    pub fn replace_after_root(&self, index: usize, node: XmlNode) -> Result<XmlNode, XmlDomError> {
        replace_document_misc(&mut self.inner.borrow_mut(), index, node, true)
    }

    /// Computes element, attribute, and node counts for the document-element tree.
    ///
    /// An unedited compact document returns its stored parse-time counts in O(1). Sparse-overlay
    /// documents are recounted because edits can change every component independently.
    ///
    /// Use [`Self::document_stats`] when document-level declaration, doctype, and misc nodes
    /// must also contribute to the node count.
    pub fn tree_stats(&self) -> XmlTreeStats {
        let inner = self.inner.borrow();
        match &inner.state {
            XmlDomState::Compact(document) => document.tree_stats(),
            XmlDomState::Overlay { compact, edits } => walk_overlay_stats(compact, edits),
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Computes counts for the complete document, including declaration, doctype, and top-level
    /// comments or processing instructions as well as the document-element tree.
    pub fn document_stats(&self) -> XmlTreeStats {
        let inner = self.inner.borrow();
        let (mut stats, mut checksum) = match &inner.state {
            XmlDomState::Compact(document) => (document.tree_stats(), 0usize),
            XmlDomState::Overlay { compact, edits } => (walk_overlay_stats(compact, edits), 0usize),
            XmlDomState::Transition => unreachable!(),
        };
        match &inner.state {
            XmlDomState::Compact(document) => {
                if let Some(declaration) = &document.metadata.declaration {
                    stats.nodes += 1;
                    checksum = checksum
                        .wrapping_add(declaration.target.len())
                        .wrapping_add(declaration.data.len());
                }
                if let Some(doctype) = &document.metadata.doctype {
                    stats.nodes += 1;
                    checksum = checksum.wrapping_add(doctype.name.len());
                }
                for node in document
                    .metadata
                    .misc_before_root
                    .iter()
                    .chain(&document.metadata.misc_after_root)
                {
                    count_materialized_node(node, &mut stats, &mut checksum);
                }
            }
            XmlDomState::Overlay { compact, edits } => {
                let declaration = edits
                    .declaration
                    .as_ref()
                    .map_or(compact.metadata.declaration.as_ref(), Option::as_ref);
                if let Some(declaration) = declaration {
                    stats.nodes += 1;
                    checksum = checksum
                        .wrapping_add(declaration.target.len())
                        .wrapping_add(declaration.data.len());
                }
                let doctype = edits
                    .doctype
                    .as_ref()
                    .map_or(compact.metadata.doctype.as_ref(), Option::as_ref);
                if let Some(doctype) = doctype {
                    stats.nodes += 1;
                    checksum = checksum.wrapping_add(doctype.name.len());
                }
                let before = edits
                    .misc_before_root
                    .as_deref()
                    .unwrap_or(&compact.metadata.misc_before_root);
                let after = edits
                    .misc_after_root
                    .as_deref()
                    .unwrap_or(&compact.metadata.misc_after_root);
                for node in before.iter().chain(after) {
                    count_materialized_node(node, &mut stats, &mut checksum);
                }
            }
            XmlDomState::Transition => unreachable!(),
        }
        std::hint::black_box(checksum);
        stats
    }

    /// Selects elements with an inline XPath expression.
    ///
    /// XPath does not change the facade's representation. Non-element members of a selected node
    /// set are filtered out, matching [`crate::XmlElement::select_elements`]. Use
    /// [`Self::select_nodes`] when attributes, text, or namespaces are required.
    pub fn select_elements(&self, query: &str) -> Result<XmlDomNodeSet, XmlDomError> {
        let inner = self.inner.borrow();
        let generation = inner.generation;
        let paths = match &inner.state {
            XmlDomState::Compact(compact) => {
                if let Some(batch) = simple_descendant_filter(query)?
                    .as_ref()
                    .and_then(|filter| compact_simple_descendant_locations(compact, filter))
                {
                    return Ok(batch.into_node_set(&self.inner, generation));
                }
                let edits = SparseOverlay::default();
                let arena = build_xpath_arena(compact, &edits)?;
                arena_element_paths(&arena, arena.select_elements(query, None)?)?
            }
            XmlDomState::Overlay { compact, edits } => {
                let arena = build_xpath_arena(compact, edits)?;
                arena_element_paths(&arena, arena.select_elements(query, None)?)?
            }
            XmlDomState::Transition => unreachable!(),
        };
        drop(inner);
        Ok(XmlDomNodeSet::from_paths(&self.inner, generation, paths))
    }

    /// Selects elements with a compiled XPath expression and scalar variable bindings.
    pub fn select_elements_with_variables(
        &self,
        expression: &XPathExpression,
        variables: &XPathVariables,
    ) -> Result<XmlDomNodeSet, XmlDomError> {
        let inner = self.inner.borrow();
        let generation = inner.generation;
        let paths = match &inner.state {
            XmlDomState::Compact(compact) => {
                let direct = variables.is_empty().then(|| {
                    expression
                        .simple_descendant_filter()
                        .and_then(|filter| compact_simple_descendant_locations(compact, &filter))
                });
                if let Some(batch) = direct.flatten() {
                    return Ok(batch.into_node_set(&self.inner, generation));
                }
                let edits = SparseOverlay::default();
                let arena = build_xpath_arena(compact, &edits)?;
                let context = XPathContext {
                    variables: variables.clone(),
                    ..XPathContext::default()
                };
                arena_evaluated_element_paths(&arena, arena.evaluate(expression, None, &context)?)?
            }
            XmlDomState::Overlay { compact, edits } => {
                let arena = build_xpath_arena(compact, edits)?;
                let context = XPathContext {
                    variables: variables.clone(),
                    ..XPathContext::default()
                };
                arena_evaluated_element_paths(&arena, arena.evaluate(expression, None, &context)?)?
            }
            XmlDomState::Transition => unreachable!(),
        };
        drop(inner);
        Ok(XmlDomNodeSet::from_paths(&self.inner, generation, paths))
    }

    /// Selects elements with a compiled XPath expression, variables, and namespace bindings.
    pub fn select_elements_with_context(
        &self,
        expression: &XPathExpression,
        context: &XPathContext,
    ) -> Result<XmlDomNodeSet, XmlDomError> {
        let inner = self.inner.borrow();
        let generation = inner.generation;
        let paths = match &inner.state {
            XmlDomState::Compact(compact) => {
                let direct = (context == &XPathContext::default()).then(|| {
                    expression
                        .simple_descendant_filter()
                        .and_then(|filter| compact_simple_descendant_locations(compact, &filter))
                });
                if let Some(batch) = direct.flatten() {
                    return Ok(batch.into_node_set(&self.inner, generation));
                }
                let edits = SparseOverlay::default();
                let arena = build_xpath_arena(compact, &edits)?;
                arena_evaluated_element_paths(&arena, arena.evaluate(expression, None, context)?)?
            }
            XmlDomState::Overlay { compact, edits } => {
                let arena = build_xpath_arena(compact, edits)?;
                arena_evaluated_element_paths(&arena, arena.evaluate(expression, None, context)?)?
            }
            XmlDomState::Transition => unreachable!(),
        };
        drop(inner);
        Ok(XmlDomNodeSet::from_paths(&self.inner, generation, paths))
    }

    /// Selects every XPath node kind without converting the compact document to another DOM.
    pub fn select_nodes(&self, query: &str) -> Result<Vec<XmlDomXPathNode>, XmlDomError> {
        let inner = self.inner.borrow();
        let generation = inner.generation;
        match &inner.state {
            XmlDomState::Compact(compact) => {
                if let Some(batch) = simple_descendant_filter(query)?
                    .as_ref()
                    .and_then(|filter| compact_simple_descendant_locations(compact, filter))
                {
                    return Ok(batch.into_xpath_nodes(&self.inner, generation));
                }
                let edits = SparseOverlay::default();
                let arena = build_xpath_arena(compact, &edits)?;
                arena_xpath_nodes(
                    &arena,
                    arena.select_elements(query, None)?,
                    &self.inner,
                    generation,
                )
            }
            XmlDomState::Overlay { compact, edits } => {
                let arena = build_xpath_arena(compact, edits)?;
                arena_xpath_nodes(
                    &arena,
                    arena.select_elements(query, None)?,
                    &self.inner,
                    generation,
                )
            }
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Selects nodes with variables.
    pub fn select_nodes_with_variables(
        &self,
        expression: &XPathExpression,
        variables: &XPathVariables,
    ) -> Result<Vec<XmlDomXPathNode>, XmlDomError> {
        let context = XPathContext {
            variables: variables.clone(),
            ..XPathContext::default()
        };
        self.select_nodes_with_context(expression, &context)
    }

    /// Selects nodes with context.
    pub fn select_nodes_with_context(
        &self,
        expression: &XPathExpression,
        context: &XPathContext,
    ) -> Result<Vec<XmlDomXPathNode>, XmlDomError> {
        let inner = self.inner.borrow();
        let generation = inner.generation;
        match &inner.state {
            XmlDomState::Compact(compact) => {
                let direct = (context == &XPathContext::default()).then(|| {
                    expression
                        .simple_descendant_filter()
                        .and_then(|filter| compact_simple_descendant_locations(compact, &filter))
                });
                if let Some(batch) = direct.flatten() {
                    return Ok(batch.into_xpath_nodes(&self.inner, generation));
                }
                let edits = SparseOverlay::default();
                let arena = build_xpath_arena(compact, &edits)?;
                arena_evaluated_xpath_nodes(
                    &arena,
                    arena.evaluate(expression, None, context)?,
                    &self.inner,
                    generation,
                )
            }
            XmlDomState::Overlay { compact, edits } => {
                let arena = build_xpath_arena(compact, edits)?;
                arena_evaluated_xpath_nodes(
                    &arena,
                    arena.evaluate(expression, None, context)?,
                    &self.inner,
                    generation,
                )
            }
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Evaluates an XPath expression using XPath boolean conversion without materializing the
    /// secondary mutable tree.
    pub fn evaluate_xpath_boolean(&self, query: &str) -> Result<bool, XmlDomError> {
        let expression = XPathExpression::compile(query)?;
        self.evaluate_xpath_boolean_with_context(&expression, &XPathContext::default())
    }

    /// Evaluates xpath boolean with context.
    pub fn evaluate_xpath_boolean_with_context(
        &self,
        expression: &XPathExpression,
        context: &XPathContext,
    ) -> Result<bool, XmlDomError> {
        let inner = self.inner.borrow();
        match &inner.state {
            XmlDomState::Compact(compact) => {
                let edits = SparseOverlay::default();
                let arena = build_xpath_arena(compact, &edits)?;
                arena
                    .evaluate_boolean(expression, None, context)
                    .map_err(Into::into)
            }
            XmlDomState::Overlay { compact, edits } => build_xpath_arena(compact, edits)?
                .evaluate_boolean(expression, None, context)
                .map_err(Into::into),
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Evaluates an XPath expression using XPath numeric conversion.
    pub fn evaluate_xpath_number(&self, query: &str) -> Result<f64, XmlDomError> {
        let expression = XPathExpression::compile(query)?;
        self.evaluate_xpath_number_with_context(&expression, &XPathContext::default())
    }

    /// Evaluates xpath number with context.
    pub fn evaluate_xpath_number_with_context(
        &self,
        expression: &XPathExpression,
        context: &XPathContext,
    ) -> Result<f64, XmlDomError> {
        let inner = self.inner.borrow();
        match &inner.state {
            XmlDomState::Compact(compact) => {
                let edits = SparseOverlay::default();
                let arena = build_xpath_arena(compact, &edits)?;
                arena
                    .evaluate_number(expression, None, context)
                    .map_err(Into::into)
            }
            XmlDomState::Overlay { compact, edits } => build_xpath_arena(compact, edits)?
                .evaluate_number(expression, None, context)
                .map_err(Into::into),
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Evaluates an XPath expression using XPath string conversion.
    pub fn evaluate_xpath_string(&self, query: &str) -> Result<String, XmlDomError> {
        let expression = XPathExpression::compile(query)?;
        self.evaluate_xpath_string_with_context(&expression, &XPathContext::default())
    }

    /// Evaluates xpath string with context.
    pub fn evaluate_xpath_string_with_context(
        &self,
        expression: &XPathExpression,
        context: &XPathContext,
    ) -> Result<String, XmlDomError> {
        let inner = self.inner.borrow();
        match &inner.state {
            XmlDomState::Compact(compact) => {
                let edits = SparseOverlay::default();
                let arena = build_xpath_arena(compact, &edits)?;
                arena
                    .evaluate_string(expression, None, context)
                    .map_err(Into::into)
            }
            XmlDomState::Overlay { compact, edits } => build_xpath_arena(compact, edits)?
                .evaluate_string(expression, None, context)
                .map_err(Into::into),
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Returns to xml string.
    pub fn to_xml_string(&self) -> Result<String, XmlWriteError> {
        let inner = self.inner.borrow();
        let options = XmlSerializeOptions::default();
        match &inner.state {
            XmlDomState::Compact(compact) => {
                crate::serialize::compact_overlay_to_string_with_options(
                    compact,
                    &SparseOverlay::default(),
                    &options,
                )
            }
            XmlDomState::Overlay { compact, edits } => {
                crate::serialize::compact_overlay_to_string_with_options(compact, edits, &options)
            }
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Returns to xml string with options.
    pub fn to_xml_string_with_options(
        &self,
        options: &XmlSerializeOptions,
    ) -> Result<String, XmlWriteError> {
        if options == &XmlSerializeOptions::default() {
            return self.to_xml_string();
        }
        let inner = self.inner.borrow();
        match &inner.state {
            XmlDomState::Compact(compact) => {
                crate::serialize::compact_overlay_to_string_with_options(
                    compact,
                    &SparseOverlay::default(),
                    options,
                )
            }
            XmlDomState::Overlay { compact, edits } => {
                crate::serialize::compact_overlay_to_string_with_options(compact, edits, options)
            }
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Writes xml.
    pub fn write_xml<W: Write>(&self, mut writer: W) -> Result<(), XmlWriteError> {
        let inner = self.inner.borrow();
        match &inner.state {
            XmlDomState::Compact(compact) => crate::serialize::write_compact_overlay(
                compact,
                &SparseOverlay::default(),
                &mut writer,
            ),
            XmlDomState::Overlay { compact, edits } => {
                crate::serialize::write_compact_overlay(compact, edits, &mut writer)
            }
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Writes xml with options.
    pub fn write_xml_with_options<W: Write>(
        &self,
        mut writer: W,
        options: &XmlSerializeOptions,
    ) -> Result<(), XmlWriteError> {
        if options == &XmlSerializeOptions::default() {
            return self.write_xml(&mut writer);
        }
        let inner = self.inner.borrow();
        match &inner.state {
            XmlDomState::Compact(compact) => crate::serialize::write_compact_overlay_with_options(
                compact,
                &SparseOverlay::default(),
                &mut writer,
                options,
            ),
            XmlDomState::Overlay { compact, edits } => {
                crate::serialize::write_compact_overlay_with_options(
                    compact,
                    edits,
                    &mut writer,
                    options,
                )
            }
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Saves the value.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), XmlWriteError> {
        let file = fs::File::create(path)?;
        self.write_xml(file)
    }

    /// Saves with options.
    pub fn save_with_options(
        &self,
        path: impl AsRef<Path>,
        options: &XmlSerializeOptions,
    ) -> Result<(), XmlWriteError> {
        let file = fs::File::create(path)?;
        self.write_xml_with_options(file, options)
    }

    fn handle(&self, path: XmlPath) -> XmlDomNode {
        let mut inner = self.inner.borrow_mut();
        let local = node_local_id_at_path(&mut inner, &path)
            .expect("facade handle path resolves to a logical node");
        let id = XmlDomNodeId {
            document: inner.document_id,
            local,
        };
        let generation = inner.generation;
        let structure_epoch = inner.structure_epoch;
        drop(inner);
        XmlDomNode {
            inner: Rc::clone(&self.inner),
            path: RefCell::new(path.into()),
            id: Cell::new(id),
            generation: Cell::new(generation),
            structure_epoch: Cell::new(structure_epoch),
        }
    }
}

impl XmlDomNode {
    /// Returns this handle's immutable, document-scoped logical identity.
    pub fn id(&self) -> XmlDomNodeId {
        self.ensure_id()
            .expect("a public DOM node handle always resolves when its identity is requested")
    }

    fn ensure_id(&self) -> Result<XmlDomNodeId, XmlDomError> {
        let current = self.id.get();
        if current.local != u64::MAX {
            return Ok(current);
        }
        let path = self.path.borrow().to_path();
        let mut inner = self.inner.borrow_mut();
        if inner.generation != self.generation.get() || inner.document_id != current.document {
            return Err(XmlDomError::StaleHandle);
        }
        let local = node_local_id_at_path(&mut inner, &path).ok_or(XmlDomError::DeletedHandle)?;
        let id = XmlDomNodeId {
            document: inner.document_id,
            local,
        };
        self.id.set(id);
        self.structure_epoch.set(inner.structure_epoch);
        Ok(id)
    }

    /// Returns this node's semantic kind in both compact and edited representations.
    pub fn kind(&self) -> Result<XmlNodeKind, XmlDomError> {
        self.read_node(|node| match node {
            DomNodeRef::Compact { document, id, .. } => document
                .node(id)
                .expect("resolved compact node exists")
                .kind(),
            DomNodeRef::Materialized(XmlNodeRef::Element(_)) => XmlNodeKind::Element,
            DomNodeRef::Materialized(XmlNodeRef::Text(_)) => XmlNodeKind::Text,
            DomNodeRef::Materialized(XmlNodeRef::Comment(_)) => XmlNodeKind::Comment,
            DomNodeRef::Materialized(XmlNodeRef::Cdata(_)) => XmlNodeKind::Cdata,
            DomNodeRef::Materialized(XmlNodeRef::ProcessingInstruction(_)) => {
                XmlNodeKind::ProcessingInstruction
            }
        })
    }

    /// Returns the byte length of the element or processing-instruction name without allocating.
    pub fn name_len(&self) -> Result<Option<usize>, XmlDomError> {
        self.read_node(|node| match node {
            DomNodeRef::Compact { document, id, .. } => document.node_name(id).map(str::len),
            DomNodeRef::Materialized(XmlNodeRef::Element(element)) => Some(element.name.len()),
            DomNodeRef::Materialized(XmlNodeRef::ProcessingInstruction(pi)) => {
                Some(pi.target.len())
            }
            DomNodeRef::Materialized(_) => None,
        })
    }

    /// Copies this node and its descendants into an independent [`XmlNode`] value.
    pub fn snapshot(&self) -> Result<XmlNode, XmlDomError> {
        self.check_generation()?;
        let inner = self.inner.borrow();
        snapshot_node(&inner, &self.path.borrow())
    }

    /// Serializes only this node, without a document declaration or byte-order mark.
    pub fn to_xml_string(&self) -> Result<String, XmlDomOutputError> {
        self.to_xml_string_with_options(&XmlSerializeOptions::default())
    }

    /// Serializes only this node with explicit formatting and encoding policies.
    ///
    /// Document-only declaration and BOM options are intentionally ignored for subtree output.
    pub fn to_xml_string_with_options(
        &self,
        options: &XmlSerializeOptions,
    ) -> Result<String, XmlDomOutputError> {
        self.serialize_to_string(options, false)
    }

    /// Writes only this node, without a document declaration or byte-order mark.
    pub fn write_xml<W: Write>(&self, writer: W) -> Result<(), XmlDomOutputError> {
        self.write_xml_with_options(writer, &XmlSerializeOptions::default())
    }

    /// Writes only this node with explicit formatting and encoding policies.
    pub fn write_xml_with_options<W: Write>(
        &self,
        writer: W,
        options: &XmlSerializeOptions,
    ) -> Result<(), XmlDomOutputError> {
        self.serialize_to_writer(writer, options, false)
    }

    /// Serializes this element's children as an XML fragment without a wrapper element.
    pub fn to_inner_xml_string(&self) -> Result<String, XmlDomOutputError> {
        self.to_inner_xml_string_with_options(&XmlSerializeOptions::default())
    }

    /// Serializes this element's children as an XML fragment with explicit formatting policies.
    pub fn to_inner_xml_string_with_options(
        &self,
        options: &XmlSerializeOptions,
    ) -> Result<String, XmlDomOutputError> {
        self.serialize_to_string(options, true)
    }

    /// Writes this element's children as an XML fragment without a wrapper element.
    pub fn write_inner_xml<W: Write>(&self, writer: W) -> Result<(), XmlDomOutputError> {
        self.write_inner_xml_with_options(writer, &XmlSerializeOptions::default())
    }

    /// Writes this element's children as an XML fragment with explicit formatting policies.
    pub fn write_inner_xml_with_options<W: Write>(
        &self,
        writer: W,
        options: &XmlSerializeOptions,
    ) -> Result<(), XmlDomOutputError> {
        self.serialize_to_writer(writer, options, true)
    }

    fn serialize_to_string(
        &self,
        options: &XmlSerializeOptions,
        inner_xml: bool,
    ) -> Result<String, XmlDomOutputError> {
        self.check_generation()?;
        if inner_xml && self.kind()? != XmlNodeKind::Element {
            return Err(XmlDomError::NotElement.into());
        }
        let inner = self.inner.borrow();
        let output = match &inner.state {
            XmlDomState::Compact(compact) => {
                crate::serialize::compact_overlay_subtree_to_string_with_options(
                    compact,
                    &SparseOverlay::default(),
                    &self.path.borrow(),
                    options,
                    inner_xml,
                )?
            }
            XmlDomState::Overlay { compact, edits } => {
                crate::serialize::compact_overlay_subtree_to_string_with_options(
                    compact,
                    edits,
                    &self.path.borrow(),
                    options,
                    inner_xml,
                )?
            }
            XmlDomState::Transition => unreachable!(),
        };
        Ok(output)
    }

    fn serialize_to_writer<W: Write>(
        &self,
        writer: W,
        options: &XmlSerializeOptions,
        inner_xml: bool,
    ) -> Result<(), XmlDomOutputError> {
        self.check_generation()?;
        if inner_xml && self.kind()? != XmlNodeKind::Element {
            return Err(XmlDomError::NotElement.into());
        }
        let inner = self.inner.borrow();
        match &inner.state {
            XmlDomState::Compact(compact) => {
                crate::serialize::write_compact_overlay_subtree_with_options(
                    compact,
                    &SparseOverlay::default(),
                    &self.path.borrow(),
                    writer,
                    options,
                    inner_xml,
                )?
            }
            XmlDomState::Overlay { compact, edits } => {
                crate::serialize::write_compact_overlay_subtree_with_options(
                    compact,
                    edits,
                    &self.path.borrow(),
                    writer,
                    options,
                    inner_xml,
                )?
            }
            XmlDomState::Transition => unreachable!(),
        }
        Ok(())
    }

    /// Returns the parsed source position, or `None` for a newly constructed node.
    pub fn source_position(&self) -> Result<Option<XmlSourcePosition>, XmlDomError> {
        self.check_generation()?;
        let inner = self.inner.borrow();
        let (compact, id) = match &inner.state {
            XmlDomState::Compact(compact) => {
                let Some(id) = self
                    .path
                    .borrow()
                    .compact_id()
                    .or_else(|| compact_node_at(compact, &self.path.borrow()))
                else {
                    return Err(XmlDomError::InvalidTarget);
                };
                (compact, id)
            }
            XmlDomState::Overlay { compact, edits } => {
                let Some(id) = overlay_compact_node_at(compact, edits, &self.path.borrow()) else {
                    return Ok(None);
                };
                if overlay_path_descends_from_copy(compact, edits, &self.path.borrow()) {
                    return Ok(None);
                }
                (compact, id)
            }
            XmlDomState::Transition => unreachable!(),
        };
        let record = compact.node(id).ok_or(XmlDomError::InvalidTarget)?;
        let byte = record.name_start as usize;
        let prefix = compact
            .input
            .get(..byte)
            .ok_or(XmlDomError::InvalidTarget)?;
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let column = prefix
            .rsplit_once('\n')
            .map_or(prefix, |(_, tail)| tail)
            .chars()
            .count()
            + 1;
        Ok(Some(XmlSourcePosition {
            byte,
            line,
            column,
            coordinates: XmlSourceCoordinates::DecodedUtf8,
        }))
    }

    /// Selects elements relative to this element while preserving the facade representation.
    pub fn select_elements(&self, query: &str) -> Result<XmlDomNodeSet, XmlDomError> {
        self.check_generation()?;
        let inner = self.inner.borrow();
        let generation = inner.generation;
        let paths = match &inner.state {
            XmlDomState::Compact(compact) => {
                let edits = SparseOverlay::default();
                let arena = build_xpath_arena(compact, &edits)?;
                let context = arena
                    .element_at_path(self.path.borrow().indexes())
                    .ok_or(XmlDomError::NotElement)?;
                arena_element_paths(&arena, arena.select_elements(query, Some(context))?)?
            }
            XmlDomState::Overlay { compact, edits } => {
                let arena = build_xpath_arena(compact, edits)?;
                let context = arena
                    .element_at_path(self.path.borrow().indexes())
                    .ok_or(XmlDomError::NotElement)?;
                arena_element_paths(&arena, arena.select_elements(query, Some(context))?)?
            }
            XmlDomState::Transition => unreachable!(),
        };
        drop(inner);
        Ok(XmlDomNodeSet::from_paths(&self.inner, generation, paths))
    }

    /// Selects elements relative to this element with a compiled expression and scalar variables.
    pub fn select_elements_with_variables(
        &self,
        expression: &XPathExpression,
        variables: &XPathVariables,
    ) -> Result<XmlDomNodeSet, XmlDomError> {
        let context = XPathContext {
            variables: variables.clone(),
            ..XPathContext::default()
        };
        self.select_elements_with_context(expression, &context)
    }

    /// Selects elements relative to this element with a compiled expression and full context.
    ///
    /// Namespace prefixes and scalar variables come from `context`. Non-element results are
    /// filtered out; use [`Self::select_nodes_with_context`] when attributes, text, comments,
    /// processing instructions, or namespace nodes are required.
    pub fn select_elements_with_context(
        &self,
        expression: &XPathExpression,
        context: &XPathContext,
    ) -> Result<XmlDomNodeSet, XmlDomError> {
        self.check_generation()?;
        let inner = self.inner.borrow();
        let generation = inner.generation;
        let paths = match &inner.state {
            XmlDomState::Compact(compact) => {
                let edits = SparseOverlay::default();
                let arena = build_xpath_arena(compact, &edits)?;
                let context_node = arena
                    .element_at_path(self.path.borrow().indexes())
                    .ok_or(XmlDomError::NotElement)?;
                arena_evaluated_element_paths(
                    &arena,
                    arena.evaluate(expression, Some(context_node), context)?,
                )?
            }
            XmlDomState::Overlay { compact, edits } => {
                let arena = build_xpath_arena(compact, edits)?;
                let context_node = arena
                    .element_at_path(self.path.borrow().indexes())
                    .ok_or(XmlDomError::NotElement)?;
                arena_evaluated_element_paths(
                    &arena,
                    arena.evaluate(expression, Some(context_node), context)?,
                )?
            }
            XmlDomState::Transition => unreachable!(),
        };
        drop(inner);
        Ok(XmlDomNodeSet::from_paths(&self.inner, generation, paths))
    }

    /// Selects every XPath node kind relative to this element.
    pub fn select_nodes(&self, query: &str) -> Result<Vec<XmlDomXPathNode>, XmlDomError> {
        self.check_generation()?;
        let inner = self.inner.borrow();
        let generation = inner.generation;
        match &inner.state {
            XmlDomState::Compact(compact) => {
                let edits = SparseOverlay::default();
                let arena = build_xpath_arena(compact, &edits)?;
                let context = arena
                    .element_at_path(self.path.borrow().indexes())
                    .ok_or(XmlDomError::NotElement)?;
                arena_xpath_nodes(
                    &arena,
                    arena.select_elements(query, Some(context))?,
                    &self.inner,
                    generation,
                )
            }
            XmlDomState::Overlay { compact, edits } => {
                let arena = build_xpath_arena(compact, edits)?;
                let context = arena
                    .element_at_path(self.path.borrow().indexes())
                    .ok_or(XmlDomError::NotElement)?;
                arena_xpath_nodes(
                    &arena,
                    arena.select_elements(query, Some(context))?,
                    &self.inner,
                    generation,
                )
            }
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Selects every XPath node kind relative to this element with scalar variables.
    pub fn select_nodes_with_variables(
        &self,
        expression: &XPathExpression,
        variables: &XPathVariables,
    ) -> Result<Vec<XmlDomXPathNode>, XmlDomError> {
        let context = XPathContext {
            variables: variables.clone(),
            ..XPathContext::default()
        };
        self.select_nodes_with_context(expression, &context)
    }

    /// Selects every XPath node kind relative to this element with a compiled expression and
    /// full context.
    pub fn select_nodes_with_context(
        &self,
        expression: &XPathExpression,
        context: &XPathContext,
    ) -> Result<Vec<XmlDomXPathNode>, XmlDomError> {
        self.check_generation()?;
        let inner = self.inner.borrow();
        let generation = inner.generation;
        match &inner.state {
            XmlDomState::Compact(compact) => {
                let edits = SparseOverlay::default();
                let arena = build_xpath_arena(compact, &edits)?;
                let context_node = arena
                    .element_at_path(self.path.borrow().indexes())
                    .ok_or(XmlDomError::NotElement)?;
                arena_evaluated_xpath_nodes(
                    &arena,
                    arena.evaluate(expression, Some(context_node), context)?,
                    &self.inner,
                    generation,
                )
            }
            XmlDomState::Overlay { compact, edits } => {
                let arena = build_xpath_arena(compact, edits)?;
                let context_node = arena
                    .element_at_path(self.path.borrow().indexes())
                    .ok_or(XmlDomError::NotElement)?;
                arena_evaluated_xpath_nodes(
                    &arena,
                    arena.evaluate(expression, Some(context_node), context)?,
                    &self.inner,
                    generation,
                )
            }
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Returns the string value of the first node selected relative to this element.
    pub fn select_string(&self, query: &str) -> Result<Option<String>, XmlDomError> {
        self.check_generation()?;
        let inner = self.inner.borrow();
        match &inner.state {
            XmlDomState::Compact(compact) => {
                let edits = SparseOverlay::default();
                let arena = build_xpath_arena(compact, &edits)?;
                let context = arena
                    .element_at_path(self.path.borrow().indexes())
                    .ok_or(XmlDomError::NotElement)?;
                arena.select_string(query, context).map_err(Into::into)
            }
            XmlDomState::Overlay { compact, edits } => {
                let arena = build_xpath_arena(compact, edits)?;
                let context = arena
                    .element_at_path(self.path.borrow().indexes())
                    .ok_or(XmlDomError::NotElement)?;
                arena.select_string(query, context).map_err(Into::into)
            }
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Evaluates an XPath expression relative to this element using XPath boolean conversion.
    pub fn evaluate_xpath_boolean(&self, query: &str) -> Result<bool, XmlDomError> {
        let expression = XPathExpression::compile(query)?;
        self.evaluate_xpath_boolean_with_context(&expression, &XPathContext::default())
    }

    /// Evaluates a compiled XPath expression relative to this element as a boolean.
    pub fn evaluate_xpath_boolean_with_context(
        &self,
        expression: &XPathExpression,
        context: &XPathContext,
    ) -> Result<bool, XmlDomError> {
        self.check_generation()?;
        let inner = self.inner.borrow();
        match &inner.state {
            XmlDomState::Compact(compact) => {
                let edits = SparseOverlay::default();
                let arena = build_xpath_arena(compact, &edits)?;
                let context_node = arena
                    .element_at_path(self.path.borrow().indexes())
                    .ok_or(XmlDomError::NotElement)?;
                arena
                    .evaluate_boolean(expression, Some(context_node), context)
                    .map_err(Into::into)
            }
            XmlDomState::Overlay { compact, edits } => {
                let arena = build_xpath_arena(compact, edits)?;
                let context_node = arena
                    .element_at_path(self.path.borrow().indexes())
                    .ok_or(XmlDomError::NotElement)?;
                arena
                    .evaluate_boolean(expression, Some(context_node), context)
                    .map_err(Into::into)
            }
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Evaluates an XPath expression relative to this element using XPath numeric conversion.
    pub fn evaluate_xpath_number(&self, query: &str) -> Result<f64, XmlDomError> {
        let expression = XPathExpression::compile(query)?;
        self.evaluate_xpath_number_with_context(&expression, &XPathContext::default())
    }

    /// Evaluates a compiled XPath expression relative to this element as a number.
    pub fn evaluate_xpath_number_with_context(
        &self,
        expression: &XPathExpression,
        context: &XPathContext,
    ) -> Result<f64, XmlDomError> {
        self.check_generation()?;
        let inner = self.inner.borrow();
        match &inner.state {
            XmlDomState::Compact(compact) => {
                let edits = SparseOverlay::default();
                let arena = build_xpath_arena(compact, &edits)?;
                let context_node = arena
                    .element_at_path(self.path.borrow().indexes())
                    .ok_or(XmlDomError::NotElement)?;
                arena
                    .evaluate_number(expression, Some(context_node), context)
                    .map_err(Into::into)
            }
            XmlDomState::Overlay { compact, edits } => {
                let arena = build_xpath_arena(compact, edits)?;
                let context_node = arena
                    .element_at_path(self.path.borrow().indexes())
                    .ok_or(XmlDomError::NotElement)?;
                arena
                    .evaluate_number(expression, Some(context_node), context)
                    .map_err(Into::into)
            }
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Evaluates an XPath expression relative to this element using XPath string conversion.
    pub fn evaluate_xpath_string(&self, query: &str) -> Result<String, XmlDomError> {
        let expression = XPathExpression::compile(query)?;
        self.evaluate_xpath_string_with_context(&expression, &XPathContext::default())
    }

    /// Evaluates a compiled XPath expression relative to this element as a string.
    pub fn evaluate_xpath_string_with_context(
        &self,
        expression: &XPathExpression,
        context: &XPathContext,
    ) -> Result<String, XmlDomError> {
        self.check_generation()?;
        let inner = self.inner.borrow();
        match &inner.state {
            XmlDomState::Compact(compact) => {
                let edits = SparseOverlay::default();
                let arena = build_xpath_arena(compact, &edits)?;
                let context_node = arena
                    .element_at_path(self.path.borrow().indexes())
                    .ok_or(XmlDomError::NotElement)?;
                arena
                    .evaluate_string(expression, Some(context_node), context)
                    .map_err(Into::into)
            }
            XmlDomState::Overlay { compact, edits } => {
                let arena = build_xpath_arena(compact, edits)?;
                let context_node = arena
                    .element_at_path(self.path.borrow().indexes())
                    .ok_or(XmlDomError::NotElement)?;
                arena
                    .evaluate_string(expression, Some(context_node), context)
                    .map_err(Into::into)
            }
            XmlDomState::Transition => unreachable!(),
        }
    }

    /// Returns name.
    pub fn name(&self) -> Result<Option<String>, XmlDomError> {
        self.read_node(|node| match node {
            DomNodeRef::Compact {
                document,
                id,
                overlay,
            } => overlay
                .and_then(|(edits, path)| edits.names.get(path).cloned())
                .or_else(|| document.node_name(id).map(str::to_owned)),
            DomNodeRef::Materialized(XmlNodeRef::Element(element)) => Some(element.name.clone()),
            DomNodeRef::Materialized(XmlNodeRef::ProcessingInstruction(pi)) => {
                Some(pi.target.clone())
            }
            DomNodeRef::Materialized(_) => None,
        })
    }

    /// Returns the namespace-expanded name of this element.
    ///
    /// The lookup follows the element's current ancestors, so a retained handle reflects namespace
    /// bindings after moves and edits. Non-element nodes return `Ok(None)`. The returned value is
    /// owned and does not keep the document borrowed.
    pub fn expanded_name(&self) -> Result<Option<crate::XmlExpandedName>, XmlDomError> {
        if self.kind()? != XmlNodeKind::Element {
            return Ok(None);
        }
        let qualified = self.name()?.ok_or(XmlDomError::InvalidTarget)?;
        let parsed = crate::XmlQualifiedName::parse(&qualified)?;
        let prefix = parsed.prefix.map(str::to_owned);
        let local = parsed.local.to_owned();
        let namespace_uri = self.lookup_namespace_uri(parsed.prefix)?;
        Ok(Some(crate::XmlExpandedName {
            qualified,
            prefix,
            local,
            namespace_uri,
        }))
    }

    /// Resolves a namespace prefix in this node's current scope.
    ///
    /// Pass `None` for the default namespace. For non-element nodes, lookup begins at the parent
    /// element. Empty namespace declarations are treated as unbound. The reserved `xml` and
    /// `xmlns` prefixes always resolve to their standard URIs.
    pub fn lookup_namespace_uri(
        &self,
        prefix: Option<&str>,
    ) -> Result<Option<String>, XmlDomError> {
        self.check_generation()?;
        if prefix == Some("xml") {
            return Ok(Some(XML_NAMESPACE_URI.to_owned()));
        }
        if prefix == Some("xmlns") {
            return Ok(Some(XMLNS_NAMESPACE_URI.to_owned()));
        }
        let declaration =
            prefix.map_or_else(|| "xmlns".to_owned(), |prefix| format!("xmlns:{prefix}"));
        let mut current = if self.kind()? == XmlNodeKind::Element {
            Some(self.clone())
        } else {
            self.parent()?
        };
        while let Some(element) = current {
            if let Some(uri) = element.attribute(&declaration)? {
                return Ok((!uri.is_empty()).then_some(uri));
            }
            current = element.parent()?;
        }
        Ok(None)
    }

    /// Returns the first child element with the requested namespace URI and local name.
    ///
    /// Namespace declarations are resolved against each child's current position. This operation
    /// allocates only the returned handle and temporary owned names used by the facade.
    pub fn child_ns(
        &self,
        namespace_uri: Option<&str>,
        local_name: &str,
    ) -> Result<Option<Self>, XmlDomError> {
        for child in self.children()? {
            let Some(name) = child.expanded_name()? else {
                continue;
            };
            if name.local == local_name && name.namespace_uri.as_deref() == namespace_uri {
                return Ok(Some(child));
            }
        }
        Ok(None)
    }

    /// Returns child elements with the requested namespace URI and local name in document order.
    ///
    /// As with [`Self::children_named`], matches are collected into an owned iterator so no
    /// `RefCell` borrow is held while the caller iterates or mutates the document.
    pub fn children_ns(
        &self,
        namespace_uri: Option<&str>,
        local_name: &str,
    ) -> Result<std::vec::IntoIter<Self>, XmlDomError> {
        let mut matches = Vec::new();
        for child in self.children()? {
            let Some(name) = child.expanded_name()? else {
                continue;
            };
            if name.local == local_name && name.namespace_uri.as_deref() == namespace_uri {
                matches.push(child);
            }
        }
        Ok(matches.into_iter())
    }

    /// Returns an attribute value by namespace URI and local name.
    ///
    /// The default namespace never applies to unprefixed attributes. Namespace declaration
    /// attributes themselves are excluded; use [`Self::lookup_namespace_uri`] to inspect bindings.
    pub fn attribute_ns(
        &self,
        namespace_uri: Option<&str>,
        local_name: &str,
    ) -> Result<Option<String>, XmlDomError> {
        for (qualified, value) in self.attributes()? {
            if qualified == "xmlns" || qualified.starts_with("xmlns:") {
                continue;
            }
            let parsed = crate::XmlQualifiedName::parse(&qualified)?;
            if parsed.local != local_name {
                continue;
            }
            let resolved = match parsed.prefix {
                Some(prefix) => self.lookup_namespace_uri(Some(prefix))?,
                None => None,
            };
            if resolved.as_deref() == namespace_uri {
                return Ok(Some(value));
            }
        }
        Ok(None)
    }

    /// Renames an element or processing instruction without materializing the document.
    pub fn set_name(&self, name: impl Into<String>) -> Result<(), XmlDomError> {
        self.check_generation()?;
        let name = validate_mutation_name(name.into())?;
        self.prepare_path_for_mutation();
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        match &mut inner.state {
            XmlDomState::Overlay { compact, edits } => {
                if let Some(id) = overlay_compact_node_at(compact, edits, &self.path.borrow()) {
                    let kind = compact.node(id).ok_or(XmlDomError::InvalidTarget)?.kind();
                    if !matches!(
                        kind,
                        XmlNodeKind::Element | XmlNodeKind::ProcessingInstruction
                    ) {
                        return Err(XmlDomError::InvalidTarget);
                    }
                    edits.names.insert(self.path.borrow().to_path(), name);
                } else {
                    match overlay_materialized_node_mut(compact, edits, &self.path.borrow())
                        .ok_or(XmlDomError::InvalidTarget)?
                    {
                        XmlNode::Element(element) => element.name = name,
                        XmlNode::ProcessingInstruction(pi) => pi.target = name,
                        _ => return Err(XmlDomError::InvalidTarget),
                    }
                }
                edits.mutations += 1;
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        }
        Ok(())
    }

    /// Returns the direct scalar value of text, CDATA, comment, or processing-instruction nodes.
    pub fn value(&self) -> Result<Option<String>, XmlDomError> {
        self.read_node(|node| match node {
            DomNodeRef::Compact {
                document,
                id,
                overlay,
            } => {
                if let Some(value) = overlay.and_then(|(edits, path)| edits.values.get(path)) {
                    return Ok(Some(value.clone()));
                }
                let record = document.node(id).ok_or(XmlDomError::InvalidTarget)?;
                let raw = match record.kind() {
                    XmlNodeKind::Text | XmlNodeKind::Comment | XmlNodeKind::Cdata => {
                        document.input.get(
                            record.name_start as usize
                                ..(record.name_start + record.name_len) as usize,
                        )
                    }
                    XmlNodeKind::ProcessingInstruction => document.input.get(
                        record.attribute_start as usize
                            ..(record.attribute_start + record.attribute_count) as usize,
                    ),
                    XmlNodeKind::Element => None,
                };
                raw.map(|value| {
                    crate::parser::decode_compact_lexeme(
                        value,
                        if record.kind() == XmlNodeKind::Text {
                            crate::parser::CompactLexemeKind::Text
                        } else {
                            crate::parser::CompactLexemeKind::Opaque
                        },
                        document.xml11,
                        document.config.attribute_whitespace,
                    )
                    .map_err(XmlDomError::from)
                })
                .transpose()
            }
            DomNodeRef::Materialized(node) => Ok(match node {
                XmlNodeRef::Element(_) => None,
                XmlNodeRef::Text(value) | XmlNodeRef::Comment(value) | XmlNodeRef::Cdata(value) => {
                    Some(value.to_owned())
                }
                XmlNodeRef::ProcessingInstruction(pi) => Some(pi.data.clone()),
            }),
        })?
    }

    /// Updates the direct scalar value of text, CDATA, comment, or processing-instruction nodes.
    pub fn set_value(&self, value: impl Into<String>) -> Result<(), XmlDomError> {
        self.check_generation()?;
        let value = value.into();
        crate::mutation::validate_node_value(self.kind()?, &value)?;
        self.prepare_path_for_mutation();
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        match &mut inner.state {
            XmlDomState::Overlay { compact, edits } => {
                if let Some(id) = overlay_compact_node_at(compact, edits, &self.path.borrow()) {
                    if compact
                        .node(id)
                        .is_none_or(|node| node.kind() == XmlNodeKind::Element)
                    {
                        return Err(XmlDomError::InvalidTarget);
                    }
                    edits.values.insert(self.path.borrow().to_path(), value);
                } else {
                    set_materialized_node_value(
                        overlay_materialized_node_mut(compact, edits, &self.path.borrow())
                            .ok_or(XmlDomError::InvalidTarget)?,
                        value,
                    )?;
                }
                edits.mutations += 1;
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        }
        Ok(())
    }

    /// Returns attribute.
    pub fn attribute(&self, name: &str) -> Result<Option<String>, XmlDomError> {
        self.read_node(|node| match node {
            DomNodeRef::Compact {
                document,
                id,
                overlay,
            } => {
                if let Some((edits, path)) = overlay {
                    if let Some(value) = edits.attributes.get(&(path.clone(), name.to_owned())) {
                        return Ok(Some(value.clone()));
                    }
                }
                let Some(record) = document.node(id) else {
                    return Ok(None);
                };
                if record.kind() != XmlNodeKind::Element {
                    return Ok(None);
                }
                if let Some((edits, path)) = overlay {
                    if let Some(attributes) = edits.attribute_orders.get(path) {
                        for attribute in attributes {
                            match attribute {
                                SparseAttribute::Compact(index)
                                    if document.attribute_name(*index) == Some(name) =>
                                {
                                    if let Some(value) =
                                        edits.attributes.get(&(path.clone(), name.to_owned()))
                                    {
                                        return Ok(Some(value.clone()));
                                    }
                                    return document
                                        .attribute_value(*index)
                                        .map(|value| {
                                            crate::parser::decode_compact_lexeme(
                                                value,
                                                crate::parser::CompactLexemeKind::Attribute,
                                                document.xml11,
                                                document.config.attribute_whitespace,
                                            )
                                            .map_err(XmlDomError::from)
                                        })
                                        .transpose();
                                }
                                SparseAttribute::Materialized(attribute)
                                    if attribute.name == name =>
                                {
                                    return Ok(Some(attribute.value.clone()));
                                }
                                _ => {}
                            }
                        }
                        return Ok(None);
                    }
                }
                let raw = record.attribute_range().find_map(|index| {
                    (document.attribute_name(index) == Some(name))
                        .then(|| document.attribute_value(index))
                        .flatten()
                });
                raw.map(|value| {
                    crate::parser::decode_compact_lexeme(
                        value,
                        crate::parser::CompactLexemeKind::Attribute,
                        document.xml11,
                        document.config.attribute_whitespace,
                    )
                    .map_err(XmlDomError::from)
                })
                .transpose()
            }
            DomNodeRef::Materialized(XmlNodeRef::Element(element)) => Ok(element
                .attribute(name)
                .map(|attribute| attribute.value.clone())),
            DomNodeRef::Materialized(_) => Ok(None),
        })?
    }

    /// Parses an attribute while preserving `T`'s structured parse error.
    pub fn parse_attribute<T: FromStr>(
        &self,
        name: &str,
    ) -> Result<Option<T>, crate::XmlValueError<T::Err>> {
        self.attribute(name)
            .map_err(crate::XmlValueError::Access)?
            .map(|value| value.parse().map_err(crate::XmlValueError::Parse))
            .transpose()
    }

    /// Returns attributes in their current document order.
    pub fn attributes(&self) -> Result<std::vec::IntoIter<(String, String)>, XmlDomError> {
        self.read_node(|node| match node {
            DomNodeRef::Compact {
                document,
                id,
                overlay,
            } => collect_compact_attributes(document, id, overlay),
            DomNodeRef::Materialized(XmlNodeRef::Element(element)) => Ok(element
                .attributes
                .iter()
                .map(|attribute| (attribute.name.clone(), attribute.value.clone()))
                .collect()),
            DomNodeRef::Materialized(_) => Err(XmlDomError::NotElement),
        })?
        .map(Vec::into_iter)
    }

    /// Returns text.
    pub fn text(&self) -> Result<Option<String>, XmlDomError> {
        self.read_node(|node| match node {
            DomNodeRef::Compact {
                document,
                id,
                overlay,
            } => {
                if let Some((edits, path)) = overlay {
                    if let Some(value) = edits.values.get(path) {
                        return Ok(Some(value.clone()));
                    }
                }
                let Some(record) = document.node(id) else {
                    return Ok(None);
                };
                let value = match record.kind() {
                    XmlNodeKind::Element => {
                        if let Some((edits, path)) = overlay {
                            if let Some(children) = edits.child_orders.get(path) {
                                let mut found = None;
                                for (index, child) in children.iter().enumerate() {
                                    match child {
                                        SparseChild::Compact(child)
                                        | SparseChild::CompactCopy { id: child, .. } => {
                                            let kind = document.node(*child).unwrap().kind();
                                            if matches!(
                                                kind,
                                                XmlNodeKind::Text | XmlNodeKind::Cdata
                                            ) {
                                                if let Some(edited) =
                                                    edits.values.get(&path.child(index))
                                                {
                                                    return Ok(Some(edited.clone()));
                                                }
                                                found = document
                                                    .node_value(*child)
                                                    .map(|value| (value, kind));
                                                break;
                                            }
                                        }
                                        SparseChild::Materialized(
                                            XmlNode::Text(value) | XmlNode::Cdata(value),
                                        ) => {
                                            return Ok(Some(value.clone()));
                                        }
                                        SparseChild::Materialized(_) => {}
                                    }
                                }
                                found
                            } else {
                                let mut found = None;
                                for (index, child) in document.children(id).enumerate() {
                                    let kind = document.node(child).unwrap().kind();
                                    if matches!(kind, XmlNodeKind::Text | XmlNodeKind::Cdata) {
                                        if let Some(value) = edits.values.get(&path.child(index)) {
                                            return Ok(Some(value.clone()));
                                        }
                                        found =
                                            document.node_value(child).map(|value| (value, kind));
                                        break;
                                    }
                                }
                                found
                            }
                        } else {
                            document.children(id).find_map(|child| {
                                let kind = document.node(child)?.kind();
                                if matches!(kind, XmlNodeKind::Text | XmlNodeKind::Cdata) {
                                    document.node_value(child).map(|value| (value, kind))
                                } else {
                                    None
                                }
                            })
                        }
                    }
                    kind @ (XmlNodeKind::Text | XmlNodeKind::Cdata) => {
                        document.node_value(id).map(|value| (value, kind))
                    }
                    _ => None,
                };
                value
                    .map(|(value, kind)| {
                        crate::parser::decode_compact_lexeme(
                            value,
                            if kind == XmlNodeKind::Text {
                                crate::parser::CompactLexemeKind::Text
                            } else {
                                crate::parser::CompactLexemeKind::Opaque
                            },
                            document.xml11,
                            document.config.attribute_whitespace,
                        )
                        .map_err(XmlDomError::from)
                    })
                    .transpose()
            }
            DomNodeRef::Materialized(XmlNodeRef::Element(element)) => {
                Ok(element.text().map(str::to_owned))
            }
            DomNodeRef::Materialized(XmlNodeRef::Text(value) | XmlNodeRef::Cdata(value)) => {
                Ok(Some(value.to_owned()))
            }
            DomNodeRef::Materialized(_) => Ok(None),
        })?
    }

    /// Parses immediate text while preserving `T`'s structured parse error.
    pub fn parse_text<T: FromStr>(&self) -> Result<Option<T>, crate::XmlValueError<T::Err>> {
        self.text()
            .map_err(crate::XmlValueError::Access)?
            .map(|value| value.parse().map_err(crate::XmlValueError::Parse))
            .transpose()
    }

    /// Returns parent.
    pub fn parent(&self) -> Result<Option<Self>, XmlDomError> {
        self.check_generation()?;
        Ok(self
            .path
            .borrow()
            .parent()
            .map(|path| self.sibling_handle(path)))
    }

    /// Returns first child.
    pub fn first_child(&self) -> Result<Option<Self>, XmlDomError> {
        self.child_at(0)
    }

    /// Returns last child.
    pub fn last_child(&self) -> Result<Option<Self>, XmlDomError> {
        self.check_generation()?;
        let len = self.child_count()?;
        match len.checked_sub(1) {
            Some(index) => self.child_at(index),
            None => Ok(None),
        }
    }

    /// Returns child.
    pub fn child(&self, name: &str) -> Result<Option<Self>, XmlDomError> {
        self.check_generation()?;
        let count = self.child_count()?;
        for index in 0..count {
            let child = self.child_at(index)?.expect("index is inside child count");
            if child.name()?.as_deref() == Some(name) {
                return Ok(Some(child));
            }
        }
        Ok(None)
    }

    /// Returns children named.
    pub fn children_named(&self, name: &str) -> Result<std::vec::IntoIter<Self>, XmlDomError> {
        self.check_generation()?;
        let mut children = Vec::new();
        for index in 0..self.child_count()? {
            let child = self.child_at(index)?.expect("index is inside child count");
            if child.name()?.as_deref() == Some(name) {
                children.push(child);
            }
        }
        Ok(children.into_iter())
    }

    /// Returns children.
    pub fn children(&self) -> Result<std::vec::IntoIter<Self>, XmlDomError> {
        self.check_generation()?;
        let mut children = Vec::with_capacity(self.child_count()?);
        for index in 0..self.child_count()? {
            children.push(self.child_at(index)?.expect("index is inside child count"));
        }
        Ok(children.into_iter())
    }

    /// Returns this node and its descendants in document order.
    pub fn walk(&self) -> Result<XmlDomWalk, XmlDomError> {
        self.check_generation()?;
        let compact_walk = {
            let inner = self.inner.borrow();
            let path = self.path.borrow();
            match &inner.state {
                XmlDomState::Compact(document) => {
                    let start = path
                        .compact_id()
                        .or_else(|| compact_node_at(document, &path))
                        .ok_or(XmlDomError::InvalidTarget)?
                        .index();
                    let back = document
                        .node(crate::XmlViewNodeId(start))
                        .expect("resolved compact node exists")
                        .next_subtree();
                    Some((
                        inner.document_id,
                        inner.generation,
                        inner.structure_epoch,
                        start,
                        back,
                    ))
                }
                XmlDomState::Overlay { .. } => None,
                XmlDomState::Transition => unreachable!(),
            }
        };
        if let Some((document_id, generation, structure_epoch, front, back)) = compact_walk {
            return Ok(XmlDomWalk {
                storage: XmlDomWalkStorage::Compact {
                    inner: Rc::clone(&self.inner),
                    topology: Rc::new(CompactQueryTopology::new_for_walk(&self.inner)),
                    document_id,
                    generation,
                    structure_epoch,
                    front,
                    back,
                },
            });
        }

        let mut output = Vec::new();
        let mut pending = vec![self.clone()];
        while let Some(node) = pending.pop() {
            let mut children: Vec<_> = node.children()?.collect();
            children.reverse();
            pending.extend(children);
            output.push(node);
        }
        Ok(XmlDomWalk {
            storage: XmlDomWalkStorage::Materialized(output.into_iter()),
        })
    }

    /// Visits this node and its descendants in document order without retaining per-node handles.
    ///
    /// The compact-backed path borrows names and normalized scalar values directly from the
    /// source. The visitor runs while the document is immutably borrowed and therefore must not
    /// attempt to mutate this document. Use [`Self::walk`] when stable handles must escape the
    /// traversal.
    pub fn scan(
        &self,
        mut visit: impl FnMut(XmlDomScanNode<'_>) -> Result<(), XmlDomError>,
    ) -> Result<(), XmlDomError> {
        self.check_generation()?;
        {
            let inner = self.inner.borrow();
            let path = self.path.borrow();
            if let XmlDomState::Compact(document) = &inner.state {
                let start = path
                    .compact_id()
                    .or_else(|| compact_node_at(document, &path))
                    .ok_or(XmlDomError::InvalidTarget)?
                    .index();
                let back = document
                    .node(crate::XmlViewNodeId(start))
                    .expect("resolved compact node exists")
                    .next_subtree();
                for index in start..back {
                    visit(XmlDomScanNode {
                        source: XmlDomScanNodeSource::Compact {
                            document,
                            id: crate::XmlViewNodeId(index),
                        },
                    })?;
                }
                return Ok(());
            }
        }

        for node in self.walk()? {
            let kind = node.kind()?;
            let name = node.name()?;
            let value = if kind == XmlNodeKind::Element {
                None
            } else {
                node.value()?
            };
            let attributes = if kind == XmlNodeKind::Element {
                node.attributes()?.collect()
            } else {
                Vec::new()
            };
            visit(XmlDomScanNode {
                source: XmlDomScanNodeSource::Owned {
                    kind,
                    name,
                    value,
                    attributes,
                },
            })?;
        }
        Ok(())
    }

    /// Returns this element and its element descendants in document order.
    ///
    /// On an unedited compact-backed document, non-element records are skipped before stable
    /// facade handles are created. This is preferable to filtering [`Self::walk`] when only
    /// elements will be retained or inspected.
    pub fn walk_elements(&self) -> Result<XmlDomWalk, XmlDomError> {
        self.check_generation()?;
        let compact_walk = {
            let inner = self.inner.borrow();
            let path = self.path.borrow();
            match &inner.state {
                XmlDomState::Compact(document) => {
                    let start = path
                        .compact_id()
                        .or_else(|| compact_node_at(document, &path))
                        .ok_or(XmlDomError::InvalidTarget)?
                        .index();
                    if document.stats.elements == 1
                        && document
                            .node(crate::XmlViewNodeId(start))
                            .is_some_and(|node| node.kind() == XmlNodeKind::Element)
                    {
                        return Ok(XmlDomWalk {
                            storage: XmlDomWalkStorage::One(Some(self.clone())),
                        });
                    }
                    let back = document
                        .node(crate::XmlViewNodeId(start))
                        .expect("resolved compact node exists")
                        .next_subtree();
                    let selected = (start..back)
                        .filter(|&index| {
                            document
                                .node(crate::XmlViewNodeId(index))
                                .is_some_and(|node| node.kind() == XmlNodeKind::Element)
                        })
                        .map(|index| u32::try_from(index).expect("compact node id fits u32"))
                        .collect::<Vec<_>>();
                    Some((
                        inner.document_id,
                        inner.generation,
                        inner.structure_epoch,
                        selected,
                    ))
                }
                XmlDomState::Overlay { .. } => None,
                XmlDomState::Transition => unreachable!(),
            }
        };
        if let Some((document_id, generation, structure_epoch, selected)) = compact_walk {
            return Ok(XmlDomWalk {
                storage: XmlDomWalkStorage::CompactSelected {
                    inner: Rc::clone(&self.inner),
                    topology: Rc::new(CompactQueryTopology::new_for_walk(&self.inner)),
                    document_id,
                    generation,
                    structure_epoch,
                    selected: selected.into_iter(),
                },
            });
        }

        let elements = self
            .walk()?
            .filter(|node| node.kind().ok() == Some(XmlNodeKind::Element))
            .collect::<Vec<_>>();
        Ok(XmlDomWalk {
            storage: XmlDomWalkStorage::Materialized(elements.into_iter()),
        })
    }

    /// Returns descendants.
    pub fn descendants(&self) -> Result<std::vec::IntoIter<Self>, XmlDomError> {
        let mut nodes: Vec<_> = self.walk()?.collect();
        if !nodes.is_empty() {
            nodes.remove(0);
        }
        Ok(nodes.into_iter())
    }

    /// Returns this node's depth below the document element.
    ///
    /// The document element has depth zero. Like other path-dependent accessors, this refreshes a
    /// retained handle after structural edits and reports [`XmlDomError::DeletedHandle`] or
    /// [`XmlDomError::StaleHandle`] instead of reading an obsolete cached path.
    pub fn depth(&self) -> Result<usize, XmlDomError> {
        self.check_generation()?;
        Ok(self.path.borrow().indexes().len())
    }

    /// Returns next sibling.
    pub fn next_sibling(&self) -> Result<Option<Self>, XmlDomError> {
        self.sibling(true)
    }

    /// Returns previous sibling.
    pub fn previous_sibling(&self) -> Result<Option<Self>, XmlDomError> {
        self.sibling(false)
    }

    /// Returns next sibling named.
    pub fn next_sibling_named(&self, name: &str) -> Result<Option<Self>, XmlDomError> {
        let mut sibling = self.next_sibling()?;
        while let Some(node) = sibling {
            if node.name()?.as_deref() == Some(name) {
                return Ok(Some(node));
            }
            sibling = node.next_sibling()?;
        }
        Ok(None)
    }

    /// Returns previous sibling named.
    pub fn previous_sibling_named(&self, name: &str) -> Result<Option<Self>, XmlDomError> {
        let mut sibling = self.previous_sibling()?;
        while let Some(node) = sibling {
            if node.name()?.as_deref() == Some(name) {
                return Ok(Some(node));
            }
            sibling = node.previous_sibling()?;
        }
        Ok(None)
    }

    /// Returns root.
    pub fn root(&self) -> Result<Self, XmlDomError> {
        self.check_generation()?;
        Ok(self.sibling_handle(XmlPath::root()))
    }

    /// Appends node.
    pub fn append_node(&self, node: XmlNode) -> Result<Self, XmlDomError> {
        self.check_generation()?;
        crate::mutation::validate_node(&node)?;
        self.prepare_path_for_mutation();
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        let index = match &mut inner.state {
            XmlDomState::Overlay { compact, edits } => {
                edits.mutations += 1;
                if let Some(element) =
                    overlay_materialized_element_mut(compact, edits, &self.path.borrow())
                {
                    let index = element.children.len();
                    element
                        .append_child(node)
                        .expect("facade validated the appended node");
                    index
                } else if let Some(id) =
                    overlay_compact_node_at(compact, edits, &self.path.borrow())
                {
                    if compact
                        .node(id)
                        .is_none_or(|node| node.kind() != XmlNodeKind::Element)
                    {
                        return Err(XmlDomError::NotElement);
                    }
                    if let Some(children) = edits.child_orders.get_mut(&self.path.borrow()) {
                        let index = children.len();
                        children.push(SparseChild::Materialized(node));
                        index
                    } else {
                        let base = compact.children(id).count();
                        let added = edits
                            .appended
                            .entry(self.path.borrow().to_path())
                            .or_default();
                        let index = base + added.len();
                        added.push(node);
                        index
                    }
                } else {
                    return Err(XmlDomError::NotElement);
                }
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        };
        Ok(self.sibling_handle(self.path.borrow().child(index)))
    }

    /// Appends fragment.
    pub fn append_fragment(&self, fragment: crate::XmlFragment) -> Result<Vec<Self>, XmlDomError> {
        let range = self.extend_children(fragment.into_nodes())?;
        Ok(range
            .map(|index| self.sibling_handle(self.path.borrow().child(index)))
            .collect())
    }

    /// Appends a batch of materialized child nodes with one facade validation and borrow.
    ///
    /// This is the preferred construction path when callers can assemble independent subtrees
    /// before attaching them. The nodes remain in the sparse overlay; the document is not
    /// converted to a secondary document tree. The returned range contains their logical child indexes.
    pub fn extend_children<I>(&self, nodes: I) -> Result<Range<usize>, XmlDomError>
    where
        I: IntoIterator<Item = XmlNode>,
    {
        self.check_generation()?;
        self.prepare_path_for_mutation();
        let nodes: Vec<_> = nodes.into_iter().collect();
        for node in &nodes {
            crate::mutation::validate_node(node)?;
        }
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        match &mut inner.state {
            XmlDomState::Overlay { compact, edits } => {
                let count = nodes.len();
                let range = if let Some(element) =
                    overlay_materialized_element_mut(compact, edits, &self.path.borrow())
                {
                    let start = element.children.len();
                    element.children.extend(nodes);
                    start..start + count
                } else if let Some(id) =
                    overlay_compact_node_at(compact, edits, &self.path.borrow())
                {
                    if compact
                        .node(id)
                        .is_none_or(|node| node.kind() != XmlNodeKind::Element)
                    {
                        return Err(XmlDomError::NotElement);
                    }
                    if let Some(children) = edits.child_orders.get_mut(&self.path.borrow()) {
                        let start = children.len();
                        children.extend(nodes.into_iter().map(SparseChild::Materialized));
                        start..start + count
                    } else {
                        let base = compact.children(id).count();
                        let entry = edits.appended.entry(self.path.borrow().to_path());
                        let start = base
                            + match &entry {
                                std::collections::hash_map::Entry::Occupied(entry) => {
                                    entry.get().len()
                                }
                                std::collections::hash_map::Entry::Vacant(_) => 0,
                            };
                        match entry {
                            std::collections::hash_map::Entry::Occupied(mut entry) => {
                                entry.get_mut().extend(nodes);
                            }
                            std::collections::hash_map::Entry::Vacant(entry) => {
                                entry.insert(nodes);
                            }
                        }
                        start..start + count
                    }
                } else {
                    return Err(XmlDomError::NotElement);
                };
                edits.mutations += count;
                Ok(range)
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        }
    }

    /// Prepends node.
    pub fn prepend_node(&self, node: XmlNode) -> Result<Self, XmlDomError> {
        self.check_generation()?;
        crate::mutation::validate_node(&node)?;
        self.prepare_path_for_mutation();
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        match &mut inner.state {
            XmlDomState::Overlay { compact, edits } => {
                if let Some(element) =
                    overlay_materialized_element_mut(compact, edits, &self.path.borrow())
                {
                    let old_len = element.children.len();
                    element
                        .prepend_child(node)
                        .expect("facade validated the prepended node");
                    let positions = insertion_positions(old_len, 0);
                    rebase_overlay_paths(edits, &self.path.borrow(), &positions);
                } else {
                    ensure_child_order(compact, edits, &self.path.borrow())?;
                    let old = compact_child_identity(&edits.child_orders[&self.path.borrow()]);
                    edits
                        .child_orders
                        .get_mut(&self.path.borrow())
                        .expect("child order was initialized")
                        .insert(0, SparseChild::Materialized(node));
                    rebase_after_child_order_change(
                        edits,
                        &self.path.borrow(),
                        &old,
                        &insertion_positions(old.len(), 0),
                    );
                }
                edits.mutations += 1;
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        }
        let generation = bump_structure_epoch(&mut inner);
        self.structure_epoch.set(generation);
        Ok(self.fresh_handle(self.path.borrow().child(0), generation))
    }

    /// Inserts before.
    pub fn insert_before(&self, node: XmlNode) -> Result<Self, XmlDomError> {
        self.insert_sibling(node, false)
    }

    /// Inserts after.
    pub fn insert_after(&self, node: XmlNode) -> Result<Self, XmlDomError> {
        self.insert_sibling(node, true)
    }

    /// Replaces a value.
    pub fn replace(&self, node: XmlNode) -> Result<XmlNode, XmlDomError> {
        self.check_generation()?;
        crate::mutation::validate_node(&node)?;
        let parent = self
            .path
            .borrow()
            .parent()
            .ok_or(XmlDomError::RootHasNoSiblings)?;
        let index = *self
            .path
            .borrow()
            .indexes()
            .last()
            .expect("non-root path has index");
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        let replaced = match &mut inner.state {
            XmlDomState::Overlay { compact, edits } => {
                if let Some(element) = overlay_materialized_element_mut(compact, edits, &parent) {
                    let old_len = element.children.len();
                    let replaced = element
                        .replace_child(index, node)
                        .map_err(|_| XmlDomError::InvalidTarget)?;
                    let positions = replacement_positions(old_len, index);
                    rebase_overlay_paths(edits, &parent, &positions);
                    replaced
                } else {
                    let replaced = materialize_overlay_node(compact, edits, &self.path.borrow())?;
                    ensure_child_order(compact, edits, &parent)?;
                    let old = compact_child_identity(&edits.child_orders[&parent]);
                    let slot = edits
                        .child_orders
                        .get_mut(&parent)
                        .and_then(|children| children.get_mut(index))
                        .ok_or(XmlDomError::InvalidTarget)?;
                    *slot = SparseChild::Materialized(node);
                    rebase_after_child_order_change(
                        edits,
                        &parent,
                        &old,
                        &replacement_positions(old.len(), index),
                    );
                    replaced
                }
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        };
        bump_structure_epoch(&mut inner);
        Ok(replaced)
    }

    /// Appends copy.
    pub fn append_copy(&self, source: &Self) -> Result<Self, XmlDomError> {
        self.check_generation()?;
        source.check_generation()?;
        self.prepare_path_for_mutation();
        source.prepare_path_for_mutation();
        if Rc::ptr_eq(&self.inner, &source.inner) {
            let mut inner = self.inner.borrow_mut();
            ensure_overlay(&mut inner);
            if let XmlDomState::Overlay { compact, edits } = &mut inner.state {
                if let Some(source_id) =
                    overlay_compact_node_at(compact, edits, &source.path.borrow())
                        .filter(|_| !overlay_has_subtree_edits(edits, &source.path.borrow()))
                {
                    let destination_id =
                        overlay_compact_node_at(compact, edits, &self.path.borrow())
                            .ok_or(XmlDomError::NotElement)?;
                    if compact
                        .node(destination_id)
                        .is_none_or(|node| node.kind() != XmlNodeKind::Element)
                    {
                        return Err(XmlDomError::NotElement);
                    }
                    ensure_child_order(compact, edits, &self.path.borrow())?;
                    let index = edits
                        .child_orders
                        .get(&self.path.borrow())
                        .expect("copy destination child order exists")
                        .len();
                    let record = compact
                        .node(source_id)
                        .expect("resolved compact copy source exists");
                    let subtree_len = record.next_subtree() - source_id.index();
                    let identity = inner.next_node_id;
                    inner.next_node_id = inner.next_node_id.wrapping_add(subtree_len as u64);
                    let XmlDomState::Overlay { edits, .. } = &mut inner.state else {
                        unreachable!()
                    };
                    let children = edits
                        .child_orders
                        .get_mut(&self.path.borrow())
                        .expect("copy destination child order exists");
                    children.push(SparseChild::CompactCopy {
                        id: source_id,
                        identity,
                    });
                    edits.mutations += 1;
                    return Ok(self.sibling_handle(self.path.borrow().child(index)));
                }
            }
        }
        let copied = {
            let inner = source.inner.borrow();
            snapshot_node(&inner, &source.path.borrow())?
        };
        self.append_node(copied)
    }

    /// Moves to.
    pub fn move_to(&self, destination: &Self, index: usize) -> Result<Self, XmlDomError> {
        self.check_same_document(destination)?;
        self.prepare_path_for_mutation();
        destination.prepare_path_for_mutation();
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        if let XmlDomState::Overlay { compact, edits } = &mut inner.state {
            let source_parent = self.path.borrow().parent();
            let source_index = self.path.borrow().indexes().last().copied();
            if let Some(source_index) = source_index.filter(|_| {
                source_parent.as_ref() == Some(&destination.path.borrow())
                    && edits.relocations.is_empty()
                    && !edits.child_orders.contains_key(&destination.path.borrow())
                    && overlay_compact_node_at(compact, edits, &self.path.borrow()).is_some()
                    && overlay_compact_node_at(compact, edits, &destination.path.borrow())
                        .and_then(|id| compact.node(id))
                        .is_some_and(|node| node.kind() == XmlNodeKind::Element)
            }) {
                let parent_id = overlay_compact_node_at(compact, edits, &destination.path.borrow())
                    .expect("validated compact destination");
                let child_count = compact.children(parent_id).count()
                    + edits
                        .appended
                        .get(&destination.path.borrow())
                        .map_or(0, Vec::len);
                if index > child_count {
                    return Err(XmlMutationError::IndexOutOfBounds {
                        index,
                        len: child_count,
                    }
                    .into());
                }
                let positions = relocation_positions(child_count, source_index, index);
                rebase_overlay_paths(edits, &destination.path.borrow(), &positions);
                edits.relocations.push(SparseRelocation {
                    parent: destination.path.borrow().to_path(),
                    source_index,
                    destination_index: index,
                });
                edits.mutations += 1;
                let relocated_index = index - usize::from(source_index < index);
                let generation = bump_structure_epoch(&mut inner);
                return Ok(
                    self.fresh_handle(destination.path.borrow().child(relocated_index), generation)
                );
            }
            if let Some(source_index) =
                source_index.filter(|_| source_parent.as_ref() == Some(&destination.path.borrow()))
            {
                if let Some(element) =
                    overlay_materialized_element_mut(compact, edits, &destination.path.borrow())
                {
                    let child_count = element.children.len();
                    if index > child_count {
                        return Err(XmlMutationError::IndexOutOfBounds {
                            index,
                            len: child_count,
                        }
                        .into());
                    }
                    let moved = element.children.remove(source_index);
                    let relocated_index = index - usize::from(source_index < index);
                    element.children.insert(relocated_index, moved);
                    let positions = relocation_positions(child_count, source_index, index);
                    rebase_overlay_paths(edits, &destination.path.borrow(), &positions);
                    edits.mutations += 1;
                    let generation = bump_structure_epoch(&mut inner);
                    return Ok(self.fresh_handle(
                        destination.path.borrow().child(relocated_index),
                        generation,
                    ));
                }
                ensure_child_order(compact, edits, &destination.path.borrow())?;
                let old = compact_child_identity(&edits.child_orders[&destination.path.borrow()]);
                let child_count = old.len();
                if index > child_count {
                    return Err(XmlMutationError::IndexOutOfBounds {
                        index,
                        len: child_count,
                    }
                    .into());
                }
                if source_index >= child_count {
                    return Err(XmlDomError::InvalidTarget);
                }
                let children = edits
                    .child_orders
                    .get_mut(&destination.path.borrow())
                    .expect("child order was initialized");
                let moved = children.remove(source_index);
                let relocated_index = index - usize::from(source_index < index);
                children.insert(relocated_index, moved);
                edits.mutations += 1;
                rebase_after_child_order_change(
                    edits,
                    &destination.path.borrow(),
                    &old,
                    &relocation_positions(child_count, source_index, index),
                );
                let generation = bump_structure_epoch(&mut inner);
                return Ok(
                    self.fresh_handle(destination.path.borrow().child(relocated_index), generation)
                );
            }

            let Some(source_parent) = source_parent else {
                return Err(XmlMutationError::RootHasNoSiblings.into());
            };
            let source_index = source_index.expect("non-root source has a child index");
            if destination
                .path
                .borrow()
                .indexes()
                .starts_with(self.path.borrow().indexes())
            {
                return Err(XmlMutationError::MoveIntoDescendant.into());
            }
            let destination_len = match overlay_node_at(compact, edits, &destination.path.borrow())
                .ok_or(XmlMutationError::InvalidPath)?
            {
                OverlayNodeRef::Compact(id) => {
                    if compact.node(id).unwrap().kind() != XmlNodeKind::Element {
                        return Err(XmlMutationError::DestinationNotElement.into());
                    }
                    edits
                        .child_orders
                        .get(&destination.path.borrow())
                        .map_or_else(
                            || {
                                compact.children(id).count()
                                    + edits
                                        .appended
                                        .get(&destination.path.borrow())
                                        .map_or(0, Vec::len)
                            },
                            Vec::len,
                        )
                }
                OverlayNodeRef::Materialized(node) => node
                    .as_element()
                    .ok_or(XmlMutationError::DestinationNotElement)?
                    .children
                    .len(),
            };
            if index > destination_len {
                return Err(XmlMutationError::IndexOutOfBounds {
                    index,
                    len: destination_len,
                }
                .into());
            }
            if overlay_compact_node_at(compact, edits, &self.path.borrow()).is_some()
                && !overlay_has_subtree_edits(edits, &self.path.borrow())
                && overlay_compact_node_at(compact, edits, &source_parent).is_some()
                && overlay_compact_node_at(compact, edits, &destination.path.borrow()).is_some()
            {
                ensure_child_order(compact, edits, &source_parent)?;
                let old_source = compact_child_identity(&edits.child_orders[&source_parent]);
                let moved = {
                    let children = edits.child_orders.get_mut(&source_parent).unwrap();
                    if source_index >= children.len() {
                        return Err(XmlMutationError::InvalidPath.into());
                    }
                    children.remove(source_index)
                };
                rebase_after_child_order_change(
                    edits,
                    &source_parent,
                    &old_source,
                    &removal_positions(old_source.len(), source_index),
                );
                let adjusted_destination =
                    adjust_path_after_removal(&destination.path.borrow(), &self.path.borrow());
                ensure_child_order(compact, edits, &adjusted_destination)?;
                let old_destination =
                    compact_child_identity(&edits.child_orders[&adjusted_destination]);
                edits
                    .child_orders
                    .get_mut(&adjusted_destination)
                    .unwrap()
                    .insert(index, moved);
                rebase_after_child_order_change(
                    edits,
                    &adjusted_destination,
                    &old_destination,
                    &insertion_positions(old_destination.len(), index),
                );
                edits.mutations += 1;
                let generation = bump_structure_epoch(&mut inner);
                return Ok(self.fresh_handle(adjusted_destination.child(index), generation));
            }
            let moved_identities =
                capture_overlay_subtree_identities(compact, edits, &self.path.borrow());
            let moved = materialize_overlay_node(compact, edits, &self.path.borrow())?;
            if let Some(parent) = overlay_materialized_element_mut(compact, edits, &source_parent) {
                let old_len = parent.children.len();
                if source_index >= parent.children.len() {
                    return Err(XmlMutationError::InvalidPath.into());
                }
                parent.children.remove(source_index);
                rebase_overlay_paths(
                    edits,
                    &source_parent,
                    &removal_positions(old_len, source_index),
                );
            } else {
                ensure_child_order(compact, edits, &source_parent)?;
                let old = compact_child_identity(&edits.child_orders[&source_parent]);
                let children = edits.child_orders.get_mut(&source_parent).unwrap();
                if source_index >= children.len() {
                    return Err(XmlMutationError::InvalidPath.into());
                }
                children.remove(source_index);
                rebase_after_child_order_change(
                    edits,
                    &source_parent,
                    &old,
                    &removal_positions(old.len(), source_index),
                );
            }
            let adjusted_destination =
                adjust_path_after_removal(&destination.path.borrow(), &self.path.borrow());
            if let Some(parent) =
                overlay_materialized_element_mut(compact, edits, &adjusted_destination)
            {
                let old_len = parent.children.len();
                parent.children.insert(index, moved);
                rebase_overlay_paths(
                    edits,
                    &adjusted_destination,
                    &insertion_positions(old_len, index),
                );
            } else {
                ensure_child_order(compact, edits, &adjusted_destination)?;
                let old = compact_child_identity(&edits.child_orders[&adjusted_destination]);
                edits
                    .child_orders
                    .get_mut(&adjusted_destination)
                    .unwrap()
                    .insert(index, SparseChild::Materialized(moved));
                rebase_after_child_order_change(
                    edits,
                    &adjusted_destination,
                    &old,
                    &insertion_positions(old.len(), index),
                );
            }
            restore_moved_identities(
                &mut edits.identity_cache,
                &adjusted_destination.child(index),
                moved_identities,
            );
            edits.mutations += 1;
            let generation = bump_structure_epoch(&mut inner);
            return Ok(self.fresh_handle(adjusted_destination.child(index), generation));
        }
        unreachable!("ensure_overlay always leaves an overlay state")
    }

    /// Appends element.
    pub fn append_element(&self, name: impl Into<String>) -> Result<Self, XmlDomError> {
        self.check_generation()?;
        let name = validate_mutation_name(name.into())?;
        self.prepare_path_for_mutation();
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        let index = match &mut inner.state {
            XmlDomState::Overlay { compact, edits } => {
                edits.mutations += 1;
                if let Some(element) =
                    overlay_materialized_element_mut(compact, edits, &self.path.borrow())
                {
                    let index = element.children.len();
                    element.append_element_unchecked(name);
                    index
                } else if let Some(id) =
                    overlay_compact_node_at(compact, edits, &self.path.borrow())
                {
                    if compact
                        .node(id)
                        .is_none_or(|node| node.kind() != XmlNodeKind::Element)
                    {
                        return Err(XmlDomError::NotElement);
                    }
                    if let Some(children) = edits.child_orders.get_mut(&self.path.borrow()) {
                        let index = children.len();
                        children.push(SparseChild::Materialized(XmlNode::element_unchecked(name)));
                        index
                    } else {
                        let base = compact.children(id).count();
                        let added = edits
                            .appended
                            .entry(self.path.borrow().to_path())
                            .or_default();
                        let index = base + added.len();
                        added.push(XmlNode::element_unchecked(name));
                        index
                    }
                } else {
                    return Err(XmlDomError::NotElement);
                }
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        };
        Ok(self.sibling_handle(self.path.borrow().child(index)))
    }

    /// Prepends element.
    pub fn prepend_element(&self, name: impl Into<String>) -> Result<Self, XmlDomError> {
        let name = validate_mutation_name(name.into())?;
        self.prepend_node(XmlNode::element_unchecked(name))
    }

    /// Returns or creates element.
    pub fn ensure_element(&self, name: impl Into<String>) -> Result<Self, XmlDomError> {
        let name = validate_mutation_name(name.into())?;
        if let Some(child) = self.child(&name)? {
            return Ok(child);
        }
        self.append_element(name)
    }

    /// Sets attribute.
    pub fn set_attribute(
        &self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), XmlDomError> {
        self.check_generation()?;
        let name = validate_mutation_name(name.into())?;
        let value = value.into();
        crate::mutation::validate_characters(&value)?;
        self.prepare_path_for_mutation();
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        match &mut inner.state {
            XmlDomState::Overlay { compact, edits } => {
                edits.mutations += 1;
                if let Some(element) =
                    overlay_materialized_element_mut(compact, edits, &self.path.borrow())
                {
                    element.set_attribute_unchecked(name, value);
                } else if let Some(id) =
                    overlay_compact_node_at(compact, edits, &self.path.borrow())
                {
                    let Some(record) = compact.node(id) else {
                        return Err(XmlDomError::NotElement);
                    };
                    if record.kind() != XmlNodeKind::Element {
                        return Err(XmlDomError::NotElement);
                    }
                    if let Some(order) = edits.attribute_orders.get_mut(&self.path.borrow()) {
                        if let Some(attribute) = order.iter_mut().find(|attribute| {
                            sparse_attribute_name(compact, attribute) == Some(name.as_str())
                        }) {
                            match attribute {
                                SparseAttribute::Compact(_) => {
                                    edits
                                        .attributes
                                        .insert((self.path.borrow().to_path(), name), value);
                                }
                                SparseAttribute::Materialized(attribute) => attribute.value = value,
                            }
                        } else {
                            order.push(SparseAttribute::Materialized(
                                crate::XmlAttribute::new_unchecked(name, value),
                            ));
                        }
                    } else if record
                        .attribute_range()
                        .any(|index| compact.attribute_name(index) == Some(name.as_str()))
                        || edits
                            .attributes
                            .contains_key(&(self.path.borrow().to_path(), name.clone()))
                    {
                        edits
                            .attributes
                            .insert((self.path.borrow().to_path(), name), value);
                    } else {
                        edits
                            .added_attribute_order
                            .entry(self.path.borrow().to_path())
                            .or_default()
                            .push(name.clone());
                        edits
                            .attributes
                            .insert((self.path.borrow().to_path(), name), value);
                    }
                } else {
                    return Err(XmlDomError::NotElement);
                }
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        }
        Ok(())
    }

    /// Sets attribute typed.
    pub fn set_attribute_typed<T: ToXmlValue>(
        &self,
        name: impl Into<String>,
        value: T,
    ) -> Result<(), XmlDomError> {
        self.set_attribute(name, value.to_xml_value())
    }

    /// Returns or creates attribute.
    pub fn ensure_attribute(
        &self,
        name: impl Into<String>,
        default: impl Into<String>,
    ) -> Result<String, XmlDomError> {
        let name = name.into();
        if let Some(value) = self.attribute(&name)? {
            return Ok(value);
        }
        let default = default.into();
        self.set_attribute(name, default.clone())?;
        Ok(default)
    }

    /// Copies attribute from.
    pub fn copy_attribute_from(&self, source: &Self, name: &str) -> Result<bool, XmlDomError> {
        let Some(value) = source.attribute(name)? else {
            return Ok(false);
        };
        self.set_attribute(name, value)?;
        Ok(true)
    }

    /// Moves attribute from.
    pub fn move_attribute_from(&self, source: &Self, name: &str) -> Result<bool, XmlDomError> {
        self.check_generation()?;
        source.check_generation()?;
        if self.path == source.path {
            return Ok(self.attribute(name)?.is_some());
        }
        if !self.copy_attribute_from(source, name)? {
            return Ok(false);
        }
        source.remove_attribute(name)
    }

    /// Replaces attribute.
    pub fn replace_attribute(
        &self,
        old_name: &str,
        new_name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<bool, XmlDomError> {
        self.check_generation()?;
        let new_name = validate_mutation_name(new_name.into())?;
        let value = value.into();
        crate::mutation::validate_characters(&value)?;
        self.prepare_path_for_mutation();
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        match &mut inner.state {
            XmlDomState::Overlay { compact, edits } => {
                if let Some(element) =
                    overlay_materialized_element_mut(compact, edits, &self.path.borrow())
                {
                    let Some(index) = element
                        .attributes
                        .iter()
                        .position(|attribute| attribute.name == old_name)
                    else {
                        return Ok(false);
                    };
                    element.attributes[index] = crate::XmlAttribute::new_unchecked(new_name, value);
                } else {
                    ensure_attribute_order(compact, edits, &self.path.borrow())?;
                    let Some(index) =
                        edits.attribute_orders[&self.path.borrow()]
                            .iter()
                            .position(|attribute| {
                                sparse_attribute_name(compact, attribute) == Some(old_name)
                            })
                    else {
                        return Ok(false);
                    };
                    edits
                        .attributes
                        .remove(&(self.path.borrow().to_path(), old_name.to_owned()));
                    edits.attribute_orders.get_mut(&self.path.borrow()).unwrap()[index] =
                        SparseAttribute::Materialized(crate::XmlAttribute::new_unchecked(
                            new_name, value,
                        ));
                }
                edits.mutations += 1;
                Ok(true)
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        }
    }

    /// Removes attribute.
    pub fn remove_attribute(&self, name: &str) -> Result<bool, XmlDomError> {
        self.check_generation()?;
        self.prepare_path_for_mutation();
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        match &mut inner.state {
            XmlDomState::Overlay { compact, edits } => {
                let removed =
                    if let Some(element) =
                        overlay_materialized_element_mut(compact, edits, &self.path.borrow())
                    {
                        element.remove_attribute(name).is_some()
                    } else {
                        ensure_attribute_order(compact, edits, &self.path.borrow())?;
                        let Some(index) =
                            edits.attribute_orders[&self.path.borrow()].iter().position(
                                |attribute| sparse_attribute_name(compact, attribute) == Some(name),
                            )
                        else {
                            return Ok(false);
                        };
                        edits
                            .attribute_orders
                            .get_mut(&self.path.borrow())
                            .unwrap()
                            .remove(index);
                        edits
                            .attributes
                            .remove(&(self.path.borrow().to_path(), name.to_owned()));
                        true
                    };
                edits.mutations += usize::from(removed);
                Ok(removed)
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        }
    }

    /// Prepends attribute.
    pub fn prepend_attribute(
        &self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), XmlDomError> {
        self.check_generation()?;
        self.prepare_path_for_mutation();
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        insert_overlay_attribute(
            &mut inner,
            &self.path.borrow(),
            0,
            name.into(),
            value.into(),
        )
    }

    /// Inserts attribute.
    pub fn insert_attribute(
        &self,
        index: usize,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), XmlDomError> {
        self.check_generation()?;
        self.prepare_path_for_mutation();
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        insert_overlay_attribute(
            &mut inner,
            &self.path.borrow(),
            index,
            name.into(),
            value.into(),
        )
    }

    /// Clears attributes.
    pub fn clear_attributes(&self) -> Result<(), XmlDomError> {
        self.check_generation()?;
        self.prepare_path_for_mutation();
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        match &mut inner.state {
            XmlDomState::Overlay { compact, edits } => {
                if let Some(element) =
                    overlay_materialized_element_mut(compact, edits, &self.path.borrow())
                {
                    element.clear_attributes();
                } else {
                    ensure_attribute_order(compact, edits, &self.path.borrow())?;
                    edits
                        .attribute_orders
                        .get_mut(&self.path.borrow())
                        .unwrap()
                        .clear();
                    edits
                        .attributes
                        .retain(|(path, _), _| path.indexes() != self.path.borrow().indexes());
                    edits.added_attribute_order.remove(&self.path.borrow());
                }
                edits.mutations += 1;
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        }
        Ok(())
    }

    /// Sets text.
    pub fn set_text(&self, value: impl Into<String>) -> Result<(), XmlDomError> {
        self.check_generation()?;
        let value = value.into();
        crate::mutation::validate_characters(&value)?;
        self.prepare_path_for_mutation();
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        match &mut inner.state {
            XmlDomState::Overlay { compact, edits } => {
                edits.mutations += 1;
                if let Some(element) =
                    overlay_materialized_element_mut(compact, edits, &self.path.borrow())
                {
                    let old_len = element.children.len();
                    let inserts_text = element.text().is_none();
                    element.set_text_unchecked(value);
                    if inserts_text {
                        let positions = insertion_positions(old_len, 0);
                        rebase_overlay_paths(edits, &self.path.borrow(), &positions);
                        let generation = bump_structure_epoch(&mut inner);
                        self.structure_epoch.set(generation);
                    }
                } else if let Some(id) =
                    overlay_compact_node_at(compact, edits, &self.path.borrow())
                {
                    if compact
                        .node(id)
                        .is_none_or(|node| node.kind() != XmlNodeKind::Element)
                    {
                        return Err(XmlDomError::NotElement);
                    }
                    if let Some(index) =
                        first_overlay_text_child(compact, edits, id, &self.path.borrow())
                    {
                        let child_path = self.path.borrow().child(index);
                        if overlay_compact_node_at(compact, edits, &child_path).is_some() {
                            edits.values.insert(child_path, value);
                        } else {
                            set_materialized_node_value(
                                overlay_materialized_node_mut(compact, edits, &child_path)
                                    .ok_or(XmlDomError::InvalidTarget)?,
                                value,
                            )?;
                        }
                    } else {
                        ensure_child_order(compact, edits, &self.path.borrow())?;
                        let old = compact_child_identity(&edits.child_orders[&self.path.borrow()]);
                        edits
                            .child_orders
                            .get_mut(&self.path.borrow())
                            .expect("child order was initialized")
                            .insert(0, SparseChild::Materialized(XmlNode::Text(value)));
                        rebase_after_child_order_change(
                            edits,
                            &self.path.borrow(),
                            &old,
                            &insertion_positions(old.len(), 0),
                        );
                        let generation = bump_structure_epoch(&mut inner);
                        self.structure_epoch.set(generation);
                        return Ok(());
                    }
                } else {
                    return Err(XmlDomError::NotElement);
                }
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        }
        Ok(())
    }

    /// Sets text typed.
    pub fn set_text_typed<T: ToXmlValue>(&self, value: T) -> Result<(), XmlDomError> {
        self.set_text(value.to_xml_value())
    }

    /// Returns or creates text.
    pub fn ensure_text(&self, default: impl Into<String>) -> Result<String, XmlDomError> {
        if let Some(value) = self.text()? {
            return Ok(value);
        }
        let default = default.into();
        self.set_text(default.clone())?;
        Ok(default)
    }

    /// Removes this value.
    pub fn remove(&self) -> Result<XmlNode, XmlDomError> {
        self.check_generation()?;
        let Some(parent_path) = self.path.borrow().parent() else {
            return Err(XmlDomError::RootHasNoSiblings);
        };
        let index = *self
            .path
            .borrow()
            .indexes()
            .last()
            .expect("non-root path has index");
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        let removed = match &mut inner.state {
            XmlDomState::Overlay { compact, edits } => {
                if !edits.child_orders.contains_key(&parent_path) && edits.relocations.is_empty() {
                    if let Some(parent_id) = overlay_compact_node_at(compact, edits, &parent_path) {
                        let base = compact.children(parent_id).count();
                        if index >= base {
                            let appended_index = index - base;
                            let mut removed = None;
                            let mut remove_entry = false;
                            if let Some(nodes) = edits.appended.get_mut(&parent_path) {
                                if appended_index < nodes.len() {
                                    removed = Some(nodes.remove(appended_index));
                                    remove_entry = nodes.is_empty();
                                }
                            }
                            if remove_entry {
                                edits.appended.remove(&parent_path);
                            }
                            if let Some(removed) = removed {
                                let remaining_len =
                                    base + edits.appended.get(&parent_path).map_or(0, Vec::len);
                                if index == remaining_len {
                                    remove_identity_subtree(
                                        &mut edits.identity_cache,
                                        &parent_path.child(index),
                                    );
                                } else {
                                    let positions = removal_positions(remaining_len + 1, index);
                                    rebase_overlay_paths(edits, &parent_path, &positions);
                                }
                                edits.mutations += 1;
                                bump_structure_epoch(&mut inner);
                                return Ok(removed);
                            }
                        }
                    }
                }
                if let Some(element) =
                    overlay_materialized_element_mut(compact, edits, &parent_path)
                {
                    let old_len = element.children.len();
                    let removed = element
                        .remove_child_at(index)
                        .ok_or(XmlDomError::InvalidTarget)?;
                    let positions = removal_positions(old_len, index);
                    rebase_overlay_paths(edits, &parent_path, &positions);
                    removed
                } else {
                    let removed = materialize_overlay_node(compact, edits, &self.path.borrow())?;
                    ensure_child_order(compact, edits, &parent_path)?;
                    let old = compact_child_identity(&edits.child_orders[&parent_path]);
                    let children = edits
                        .child_orders
                        .get_mut(&parent_path)
                        .expect("child order was initialized");
                    if index >= children.len() {
                        return Err(XmlDomError::InvalidTarget);
                    }
                    children.remove(index);
                    rebase_after_child_order_change(
                        edits,
                        &parent_path,
                        &old,
                        &removal_positions(old.len(), index),
                    );
                    edits.mutations += 1;
                    removed
                }
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        };
        bump_structure_epoch(&mut inner);
        Ok(removed)
    }

    /// Clears this value.
    pub fn clear(&self) -> Result<(), XmlDomError> {
        self.check_generation()?;
        self.prepare_path_for_mutation();
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        match &mut inner.state {
            XmlDomState::Overlay { compact, edits } => {
                if let Some(element) =
                    overlay_materialized_element_mut(compact, edits, &self.path.borrow())
                {
                    element.clear_children();
                    rebase_overlay_paths(edits, &self.path.borrow(), &HashMap::new());
                } else {
                    ensure_child_order(compact, edits, &self.path.borrow())?;
                    let old = compact_child_identity(&edits.child_orders[&self.path.borrow()]);
                    edits
                        .child_orders
                        .get_mut(&self.path.borrow())
                        .expect("child order was initialized")
                        .clear();
                    rebase_after_child_order_change(
                        edits,
                        &self.path.borrow(),
                        &old,
                        &HashMap::new(),
                    );
                }
                edits.mutations += 1;
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        }
        let generation = bump_structure_epoch(&mut inner);
        self.structure_epoch.set(generation);
        Ok(())
    }

    fn prepare_path_for_mutation(&self) {
        let _ = self.path.borrow().to_path();
    }

    fn read_node<R>(&self, read: impl FnOnce(DomNodeRef<'_>) -> R) -> Result<R, XmlDomError> {
        self.check_generation()?;
        let inner = self.inner.borrow();
        let path = self.path.borrow();
        match &inner.state {
            XmlDomState::Compact(document) => {
                let id = path
                    .compact_id()
                    .or_else(|| compact_node_at(document, &path))
                    .ok_or(XmlDomError::InvalidTarget)?;
                Ok(read(DomNodeRef::Compact {
                    document,
                    id,
                    overlay: None,
                }))
            }
            XmlDomState::Overlay { compact, edits } => {
                match overlay_node_at(compact, edits, &path).ok_or(XmlDomError::InvalidTarget)? {
                    OverlayNodeRef::Materialized(node) => {
                        Ok(read(DomNodeRef::Materialized(XmlNodeRef::from(node))))
                    }
                    OverlayNodeRef::Compact(id) => Ok(read(DomNodeRef::Compact {
                        document: compact,
                        id,
                        overlay: Some((edits, &path)),
                    })),
                }
            }
            XmlDomState::Transition => unreachable!(),
        }
    }

    fn child_count(&self) -> Result<usize, XmlDomError> {
        self.read_node(|node| match node {
            DomNodeRef::Compact {
                document,
                id,
                overlay,
            } => {
                let base = document.children(id).count();
                overlay.map_or(base, |(edits, path)| {
                    edits.child_orders.get(path).map_or_else(
                        || base + edits.appended.get(path).map_or(0, Vec::len),
                        Vec::len,
                    )
                })
            }
            DomNodeRef::Materialized(XmlNodeRef::Element(element)) => element.children.len(),
            DomNodeRef::Materialized(_) => 0,
        })
    }

    fn child_at(&self, index: usize) -> Result<Option<Self>, XmlDomError> {
        self.check_generation()?;
        if index >= self.child_count()? {
            return Ok(None);
        }
        Ok(Some(self.sibling_handle(self.path.borrow().child(index))))
    }

    fn sibling(&self, forward: bool) -> Result<Option<Self>, XmlDomError> {
        self.check_generation()?;
        let Some(parent) = self.path.borrow().parent() else {
            return Ok(None);
        };
        let current = *self
            .path
            .borrow()
            .indexes()
            .last()
            .expect("non-root path has index");
        let index = if forward {
            current.checked_add(1)
        } else {
            current.checked_sub(1)
        };
        let Some(index) = index else { return Ok(None) };
        if index >= self.sibling_handle(parent.clone()).child_count()? {
            return Ok(None);
        }
        Ok(Some(self.sibling_handle(parent.child(index))))
    }

    fn sibling_handle(&self, path: XmlPath) -> Self {
        let (id, generation, structure_epoch) = if let Ok(mut inner) = self.inner.try_borrow_mut() {
            let local = node_local_id_at_path(&mut inner, &path)
                .expect("sibling handle path resolves to a logical node");
            (
                XmlDomNodeId {
                    document: inner.document_id,
                    local,
                },
                inner.generation,
                inner.structure_epoch,
            )
        } else {
            (
                XmlDomNodeId {
                    document: self.id.get().document,
                    local: u64::MAX,
                },
                self.generation.get(),
                self.structure_epoch.get(),
            )
        };
        Self {
            inner: Rc::clone(&self.inner),
            path: RefCell::new(path.into()),
            id: Cell::new(id),
            generation: Cell::new(generation),
            structure_epoch: Cell::new(structure_epoch),
        }
    }

    fn fresh_handle(&self, path: XmlPath, _generation: u64) -> Self {
        self.sibling_handle(path)
    }

    fn insert_sibling(&self, node: XmlNode, after: bool) -> Result<Self, XmlDomError> {
        self.check_generation()?;
        crate::mutation::validate_node(&node)?;
        let parent = self
            .path
            .borrow()
            .parent()
            .ok_or(XmlDomError::RootHasNoSiblings)?;
        let current = self
            .path
            .borrow()
            .indexes()
            .last()
            .copied()
            .ok_or(XmlDomError::RootHasNoSiblings)?;
        let index = current + usize::from(after);
        let mut inner = self.inner.borrow_mut();
        ensure_overlay(&mut inner);
        match &mut inner.state {
            XmlDomState::Overlay { compact, edits } => {
                if let Some(element) = overlay_materialized_element_mut(compact, edits, &parent) {
                    let old_len = element.children.len();
                    if index > element.children.len() {
                        return Err(XmlDomError::InvalidTarget);
                    }
                    element.children.insert(index, node);
                    let positions = insertion_positions(old_len, index);
                    rebase_overlay_paths(edits, &parent, &positions);
                } else {
                    ensure_child_order(compact, edits, &parent)?;
                    let old = compact_child_identity(&edits.child_orders[&parent]);
                    let children = edits
                        .child_orders
                        .get_mut(&parent)
                        .expect("child order was initialized");
                    if index > children.len() {
                        return Err(XmlDomError::InvalidTarget);
                    }
                    children.insert(index, SparseChild::Materialized(node));
                    rebase_after_child_order_change(
                        edits,
                        &parent,
                        &old,
                        &insertion_positions(old.len(), index),
                    );
                }
                edits.mutations += 1;
            }
            XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
        };
        let generation = bump_structure_epoch(&mut inner);
        Ok(self.fresh_handle(parent.child(index), generation))
    }

    fn check_same_document(&self, other: &Self) -> Result<(), XmlDomError> {
        self.check_generation()?;
        other.check_generation()?;
        Rc::ptr_eq(&self.inner, &other.inner)
            .then_some(())
            .ok_or(XmlDomError::WrongDocument)
    }

    fn check_generation(&self) -> Result<(), XmlDomError> {
        let id = self.ensure_id()?;
        {
            let inner = self.inner.borrow();
            if inner.generation != self.generation.get() || inner.document_id != id.document {
                return Err(XmlDomError::StaleHandle);
            }
            if inner.structure_epoch == self.structure_epoch.get() {
                return Ok(());
            }
        }
        let cached = self.path.borrow().to_path();
        let mut inner = self.inner.borrow_mut();
        let resolved = if existing_node_local_id_at_path(&inner, &cached) == Some(id.local) {
            cached
        } else {
            node_path_for_local_id(&inner, id.local).ok_or(XmlDomError::DeletedHandle)?
        };
        if let XmlDomState::Overlay { edits, .. } = &mut inner.state {
            edits
                .identity_cache
                .by_id
                .insert(id.local, resolved.clone());
            edits
                .identity_cache
                .by_path
                .insert(resolved.clone(), id.local);
        }
        *self.path.borrow_mut() = resolved.into();
        self.structure_epoch.set(inner.structure_epoch);
        Ok(())
    }
}

pub(crate) fn overlay_has_subtree_edits(edits: &SparseOverlay, prefix: &XmlPath) -> bool {
    let under = |path: &XmlPath| path.indexes().starts_with(prefix.indexes());
    edits.appended.keys().any(under)
        || edits.child_orders.keys().any(under)
        || edits.attributes.keys().any(|(path, _)| under(path))
        || edits.added_attribute_order.keys().any(under)
        || edits.attribute_orders.keys().any(under)
        || edits.names.keys().any(under)
        || edits.values.keys().any(under)
        || edits.relocations.iter().any(|edit| under(&edit.parent))
}

enum DomNodeRef<'a> {
    Compact {
        document: &'a XmlCompactDocument,
        id: crate::XmlViewNodeId,
        overlay: Option<(&'a SparseOverlay, &'a XmlPath)>,
    },
    Materialized(XmlNodeRef<'a>),
}

fn ensure_overlay(inner: &mut XmlDomInner) {
    if matches!(inner.state, XmlDomState::Compact(_)) {
        let state = std::mem::replace(&mut inner.state, XmlDomState::Transition);
        let XmlDomState::Compact(compact) = state else {
            unreachable!()
        };
        inner.state = XmlDomState::Overlay {
            compact,
            edits: Box::default(),
        };
    }
}

fn document_misc_nodes(inner: &XmlDomInner, after_root: bool) -> &[XmlNode] {
    match &inner.state {
        XmlDomState::Compact(compact) => {
            if after_root {
                &compact.metadata.misc_after_root
            } else {
                &compact.metadata.misc_before_root
            }
        }
        XmlDomState::Overlay { compact, edits } => {
            if after_root {
                edits
                    .misc_after_root
                    .as_deref()
                    .unwrap_or(&compact.metadata.misc_after_root)
            } else {
                edits
                    .misc_before_root
                    .as_deref()
                    .unwrap_or(&compact.metadata.misc_before_root)
            }
        }
        XmlDomState::Transition => unreachable!(),
    }
}

fn validate_document_misc(node: &XmlNode) -> Result<(), XmlDomError> {
    if !matches!(
        node,
        XmlNode::Comment(_) | XmlNode::ProcessingInstruction(_)
    ) {
        return Err(XmlDomError::InvalidTarget);
    }
    crate::mutation::validate_node(node)?;
    Ok(())
}

fn append_document_misc(
    inner: &mut XmlDomInner,
    node: XmlNode,
    after_root: bool,
) -> Result<(), XmlDomError> {
    validate_document_misc(&node)?;
    ensure_overlay(inner);
    match &mut inner.state {
        XmlDomState::Overlay { compact, edits } => {
            let target = if after_root {
                edits
                    .misc_after_root
                    .get_or_insert_with(|| compact.metadata.misc_after_root.clone())
            } else {
                edits
                    .misc_before_root
                    .get_or_insert_with(|| compact.metadata.misc_before_root.clone())
            };
            target.push(node);
            edits.mutations += 1;
        }
        XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
    }
    Ok(())
}

fn remove_document_misc(
    inner: &mut XmlDomInner,
    index: usize,
    after_root: bool,
) -> Result<XmlNode, XmlDomError> {
    let len = document_misc_nodes(inner, after_root).len();
    if index >= len {
        return Err(XmlMutationError::IndexOutOfBounds { index, len }.into());
    }
    ensure_overlay(inner);
    let XmlDomState::Overlay { compact, edits } = &mut inner.state else {
        unreachable!()
    };
    let adjusted_doctype_index = (!after_root)
        .then(|| {
            edits
                .doctype_before_misc_index
                .unwrap_or(compact.metadata.doctype_before_misc_index)
        })
        .flatten()
        .filter(|doctype_index| index < *doctype_index)
        .map(|doctype_index| doctype_index - 1);
    let target = if after_root {
        edits
            .misc_after_root
            .get_or_insert_with(|| compact.metadata.misc_after_root.clone())
    } else {
        edits
            .misc_before_root
            .get_or_insert_with(|| compact.metadata.misc_before_root.clone())
    };
    let removed = target.remove(index);
    if let Some(doctype_index) = adjusted_doctype_index {
        edits.doctype_before_misc_index = Some(Some(doctype_index));
    }
    edits.mutations += 1;
    Ok(removed)
}

fn replace_document_misc(
    inner: &mut XmlDomInner,
    index: usize,
    node: XmlNode,
    after_root: bool,
) -> Result<XmlNode, XmlDomError> {
    validate_document_misc(&node)?;
    let len = document_misc_nodes(inner, after_root).len();
    if index >= len {
        return Err(XmlMutationError::IndexOutOfBounds { index, len }.into());
    }
    ensure_overlay(inner);
    let XmlDomState::Overlay { compact, edits } = &mut inner.state else {
        unreachable!()
    };
    let target = if after_root {
        edits
            .misc_after_root
            .get_or_insert_with(|| compact.metadata.misc_after_root.clone())
    } else {
        edits
            .misc_before_root
            .get_or_insert_with(|| compact.metadata.misc_before_root.clone())
    };
    let replaced = std::mem::replace(&mut target[index], node);
    edits.mutations += 1;
    Ok(replaced)
}

fn bump_structure_epoch(inner: &mut XmlDomInner) -> u64 {
    inner.structure_epoch = inner.structure_epoch.wrapping_add(1);
    inner.structure_epoch
}

fn bump_generation(inner: &mut XmlDomInner) -> u64 {
    inner.generation = inner.generation.wrapping_add(1);
    inner.generation
}

fn compact_node_at(document: &XmlCompactDocument, path: &XmlPath) -> Option<crate::XmlViewNodeId> {
    let mut current = document.root();
    for &index in path.indexes() {
        current = document.children(current).nth(index)?;
    }
    Some(current)
}

fn overlay_compact_local_id_at(
    compact: &XmlCompactDocument,
    edits: &SparseOverlay,
    path: &XmlPath,
) -> Option<u64> {
    let mut id = compact.root();
    let mut current = XmlPath::root();
    let mut copy: Option<(u64, crate::XmlViewNodeId)> = None;
    for &index in path.indexes() {
        if let Some(children) = edits.child_orders.get(&current) {
            match children.get(index)? {
                SparseChild::Compact(child) => id = *child,
                SparseChild::CompactCopy {
                    id: child,
                    identity,
                } => {
                    id = *child;
                    copy = Some((*identity, *child));
                }
                SparseChild::Materialized(_) => return None,
            }
        } else {
            let source_index = edits
                .relocations
                .iter()
                .find(|relocation| relocation.parent == current)
                .map_or(index, |relocation| {
                    relocated_original_index(
                        index,
                        relocation.source_index,
                        relocation.destination_index,
                    )
                });
            let base = compact.children(id).count();
            if source_index >= base {
                return None;
            }
            id = compact.children(id).nth(source_index)?;
        }
        current.indexes_mut().push(index);
    }
    Some(copy.map_or(id.index() as u64, |(base, root)| {
        base + (id.index() - root.index()) as u64
    }))
}

fn existing_node_local_id_at_path(inner: &XmlDomInner, path: &XmlPath) -> Option<u64> {
    match &inner.state {
        XmlDomState::Compact(compact) => compact_node_at(compact, path).map(|id| id.index() as u64),
        XmlDomState::Overlay { compact, edits } => {
            overlay_compact_local_id_at(compact, edits, path)
                .or_else(|| edits.identity_cache.by_path.get(path).copied())
        }
        XmlDomState::Transition => unreachable!(),
    }
}

fn node_local_id_at_path(inner: &mut XmlDomInner, path: &XmlPath) -> Option<u64> {
    if let Some(id) = existing_node_local_id_at_path(inner, path) {
        return Some(id);
    }
    let is_materialized = match &inner.state {
        XmlDomState::Overlay { compact, edits } => {
            matches!(
                overlay_node_at(compact, edits, path),
                Some(OverlayNodeRef::Materialized(_))
            )
        }
        XmlDomState::Compact(_) => false,
        XmlDomState::Transition => unreachable!(),
    };
    if !is_materialized {
        return None;
    }
    let id = inner.next_node_id;
    inner.next_node_id = inner.next_node_id.wrapping_add(1);
    let XmlDomState::Overlay { edits, .. } = &mut inner.state else {
        unreachable!("materialized nodes only exist in the overlay")
    };
    edits.identity_cache.by_id.insert(id, path.clone());
    edits.identity_cache.by_path.insert(path.clone(), id);
    Some(id)
}

fn logical_child_count(inner: &XmlDomInner, path: &XmlPath) -> usize {
    match &inner.state {
        XmlDomState::Compact(compact) => {
            compact_node_at(compact, path).map_or(0, |id| compact.children(id).count())
        }
        XmlDomState::Overlay { compact, edits } => match overlay_node_at(compact, edits, path) {
            Some(OverlayNodeRef::Compact(id)) => edits.child_orders.get(path).map_or_else(
                || compact.children(id).count() + edits.appended.get(path).map_or(0, Vec::len),
                Vec::len,
            ),
            Some(OverlayNodeRef::Materialized(node)) => node
                .as_element()
                .map_or(0, |element| element.children.len()),
            None => 0,
        },
        XmlDomState::Transition => unreachable!(),
    }
}

fn node_path_for_local_id(inner: &XmlDomInner, target: u64) -> Option<XmlPath> {
    if let XmlDomState::Overlay { edits, .. } = &inner.state {
        if let Some(path) = edits.identity_cache.by_id.get(&target) {
            if existing_node_local_id_at_path(inner, path) == Some(target) {
                return Some(path.clone());
            }
        }
    }
    let mut pending = vec![XmlPath::root()];
    while let Some(path) = pending.pop() {
        if existing_node_local_id_at_path(inner, &path) == Some(target) {
            return Some(path);
        }
        let count = logical_child_count(inner, &path);
        pending.extend((0..count).rev().map(|index| path.child(index)));
    }
    None
}

fn capture_overlay_subtree_identities(
    compact: &XmlCompactDocument,
    edits: &SparseOverlay,
    root: &XmlPath,
) -> Vec<(u64, Vec<usize>)> {
    let mut captured = Vec::new();
    let mut pending = vec![(root.clone(), Vec::new())];
    while let Some((path, relative)) = pending.pop() {
        if let Some(id) = overlay_compact_local_id_at(compact, edits, &path)
            .or_else(|| edits.identity_cache.by_path.get(&path).copied())
        {
            captured.push((id, relative.clone()));
        }
        let child_count = match overlay_node_at(compact, edits, &path) {
            Some(OverlayNodeRef::Compact(id)) => edits.child_orders.get(&path).map_or_else(
                || compact.children(id).count() + edits.appended.get(&path).map_or(0, Vec::len),
                Vec::len,
            ),
            Some(OverlayNodeRef::Materialized(node)) => node
                .as_element()
                .map_or(0, |element| element.children.len()),
            None => 0,
        };
        pending.extend((0..child_count).rev().map(|index| {
            let mut child_relative = relative.clone();
            child_relative.push(index);
            (path.child(index), child_relative)
        }));
    }
    captured
}

fn restore_moved_identities(
    cache: &mut IdentityCache,
    destination: &XmlPath,
    captured: Vec<(u64, Vec<usize>)>,
) {
    for (id, relative) in captured {
        let mut indexes = destination.indexes().to_vec();
        indexes.extend(relative);
        let path = XmlPath::from_indexes(indexes);
        cache.by_id.insert(id, path.clone());
        cache.by_path.insert(path, id);
    }
}

fn remove_identity_subtree(cache: &mut IdentityCache, removed: &XmlPath) {
    cache
        .by_id
        .retain(|_, path| !path.indexes().starts_with(removed.indexes()));
    cache
        .by_path
        .retain(|path, _| !path.indexes().starts_with(removed.indexes()));
}

struct CompactLocationBatch {
    selected: Vec<u32>,
}

impl CompactLocationBatch {
    fn into_node_set(self, inner: &Rc<RefCell<XmlDomInner>>, generation: u64) -> XmlDomNodeSet {
        XmlDomNodeSet::from_compact(
            inner,
            generation,
            Rc::new(CompactQueryTopology::new(inner)),
            self.selected,
        )
    }

    fn into_xpath_nodes(
        self,
        inner: &Rc<RefCell<XmlDomInner>>,
        generation: u64,
    ) -> Vec<XmlDomXPathNode> {
        let Self { selected } = self;
        let topology = Rc::new(CompactQueryTopology::new(inner));
        let document = inner.borrow();
        let document_id = document.document_id;
        let structure_epoch = document.structure_epoch;
        drop(document);
        let mut output = Vec::with_capacity(selected.len());
        for node_id in selected {
            output.push(XmlDomXPathNode::Element(XmlDomNode {
                inner: Rc::clone(inner),
                path: RefCell::new(XmlDomPath::compact(CompactQueryLocation {
                    topology: Rc::clone(&topology),
                    node_id: crate::XmlViewNodeId(node_id as usize),
                })),
                id: Cell::new(XmlDomNodeId {
                    document: document_id,
                    local: u64::from(node_id),
                }),
                generation: Cell::new(generation),
                structure_epoch: Cell::new(structure_epoch),
            }));
        }
        output
    }
}

fn compact_simple_descendant_locations(
    document: &XmlCompactDocument,
    filter: &SimpleDescendantFilter,
) -> Option<CompactLocationBatch> {
    if document.has_namespace_declarations {
        return None;
    }

    let selected_capacity = if filter.required_attributes.is_empty() {
        document.tree_stats().elements.min(1_024)
    } else {
        document
            .attributes()
            .len()
            .min(document.tree_stats().elements)
    };
    let mut selected = Vec::with_capacity(selected_capacity);
    for id in document.node_ids() {
        let record = document.node(id).expect("compact node id is valid");
        if record.kind() != XmlNodeKind::Element {
            continue;
        }
        if compact_simple_descendant_matches(document, id, filter) {
            selected.push(u32::try_from(id.index()).ok()?);
        }
    }
    Some(CompactLocationBatch { selected })
}

fn compact_topology_entries(
    document: &XmlCompactDocument,
    include_all_nodes: bool,
) -> Vec<CompactTopologyEntry> {
    struct Frame {
        subtree_end: usize,
        next_child: u32,
        link: u32,
    }

    let mut entries = Vec::with_capacity(if include_all_nodes {
        document.tree_stats().nodes
    } else {
        document.tree_stats().elements
    });
    let mut stack: Vec<Frame> = Vec::new();
    for id in document.node_ids() {
        let index = id.index();
        while stack.last().is_some_and(|frame| index >= frame.subtree_end) {
            stack.pop();
        }
        let child_index = if let Some(parent) = stack.last_mut() {
            let child_index = parent.next_child;
            parent.next_child = parent
                .next_child
                .checked_add(1)
                .expect("validated compact child count fits u32");
            child_index
        } else {
            u32::MAX
        };
        let record = document.node(id).expect("compact node id is valid");
        if !include_all_nodes && record.kind() != XmlNodeKind::Element {
            continue;
        }
        let link = u32::try_from(entries.len()).expect("compact element count fits u32");
        entries.push(CompactTopologyEntry {
            node_id: u32::try_from(id.index()).expect("compact node id fits u32"),
            parent: stack.last().map_or(u32::MAX, |frame| frame.link),
            child_index,
        });
        if record.kind() == XmlNodeKind::Element {
            stack.push(Frame {
                subtree_end: record.next_subtree(),
                next_child: 0,
                link,
            });
        }
    }
    entries
}

fn compact_simple_descendant_matches(
    document: &XmlCompactDocument,
    id: crate::XmlViewNodeId,
    filter: &SimpleDescendantFilter,
) -> bool {
    let record = document.node(id).expect("compact node id is valid");
    filter
        .element_name
        .as_deref()
        .is_none_or(|name| document.node_name(id) == Some(name))
        && filter.required_attributes.iter().all(|required| {
            record
                .attribute_range()
                .any(|attribute| document.attribute_name(attribute) == Some(required))
        })
}

fn first_overlay_text_child(
    compact: &XmlCompactDocument,
    edits: &SparseOverlay,
    id: crate::XmlViewNodeId,
    path: &XmlPath,
) -> Option<usize> {
    if let Some(children) = edits.child_orders.get(path) {
        return children.iter().position(|child| match child {
            SparseChild::Compact(id) | SparseChild::CompactCopy { id, .. } => compact
                .node(*id)
                .is_some_and(|node| matches!(node.kind(), XmlNodeKind::Text | XmlNodeKind::Cdata)),
            SparseChild::Materialized(node) => matches!(node, XmlNode::Text(_) | XmlNode::Cdata(_)),
        });
    }
    if edits
        .relocations
        .iter()
        .any(|relocation| relocation.parent == *path)
    {
        return xpath_source_children(compact, edits, XPathSourceNode::Compact(id), path)
            .into_iter()
            .position(|child| xpath_source_is_text(compact, child));
    }

    let mut index = 0;
    for child in compact.children(id) {
        if compact
            .node(child)
            .is_some_and(|node| matches!(node.kind(), XmlNodeKind::Text | XmlNodeKind::Cdata))
        {
            return Some(index);
        }
        index += 1;
    }
    edits
        .appended
        .get(path)
        .into_iter()
        .flatten()
        .position(|node| matches!(node, XmlNode::Text(_) | XmlNode::Cdata(_)))
        .map(|appended| index + appended)
}

pub(crate) enum OverlayNodeRef<'a> {
    Compact(crate::XmlViewNodeId),
    Materialized(&'a XmlNode),
}

pub(crate) fn overlay_node_at<'a>(
    compact: &XmlCompactDocument,
    edits: &'a SparseOverlay,
    path: &XmlPath,
) -> Option<OverlayNodeRef<'a>> {
    let mut node = OverlayNodeRef::Compact(compact.root());
    let mut current = XmlPath::root();
    for &index in path.indexes() {
        node = match node {
            OverlayNodeRef::Compact(id) => {
                if let Some(children) = edits.child_orders.get(&current) {
                    match children.get(index)? {
                        SparseChild::Compact(id) | SparseChild::CompactCopy { id, .. } => {
                            OverlayNodeRef::Compact(*id)
                        }
                        SparseChild::Materialized(node) => OverlayNodeRef::Materialized(node),
                    }
                } else {
                    let base = compact.children(id).count();
                    let source_index = edits
                        .relocations
                        .iter()
                        .find(|relocation| relocation.parent == current)
                        .map_or(index, |relocation| {
                            relocated_original_index(
                                index,
                                relocation.source_index,
                                relocation.destination_index,
                            )
                        });
                    if source_index < base {
                        OverlayNodeRef::Compact(compact.children(id).nth(source_index)?)
                    } else {
                        OverlayNodeRef::Materialized(
                            edits.appended.get(&current)?.get(source_index - base)?,
                        )
                    }
                }
            }
            OverlayNodeRef::Materialized(node) => {
                OverlayNodeRef::Materialized(node.as_element()?.children.get(index)?)
            }
        };
        current.indexes_mut().push(index);
    }
    Some(node)
}

fn overlay_compact_node_at(
    compact: &XmlCompactDocument,
    edits: &SparseOverlay,
    path: &XmlPath,
) -> Option<crate::XmlViewNodeId> {
    match overlay_node_at(compact, edits, path)? {
        OverlayNodeRef::Compact(id) => Some(id),
        OverlayNodeRef::Materialized(_) => None,
    }
}

fn overlay_path_descends_from_copy(
    compact: &XmlCompactDocument,
    edits: &SparseOverlay,
    path: &XmlPath,
) -> bool {
    let mut id = compact.root();
    let mut current = XmlPath::root();
    let mut copied = false;
    for &index in path.indexes() {
        if let Some(children) = edits.child_orders.get(&current) {
            match children.get(index) {
                Some(SparseChild::Compact(child)) => id = *child,
                Some(SparseChild::CompactCopy { id: child, .. }) => {
                    id = *child;
                    copied = true;
                }
                Some(SparseChild::Materialized(_)) | None => return false,
            }
        } else {
            let source_index = edits
                .relocations
                .iter()
                .find(|relocation| relocation.parent == current)
                .map_or(index, |relocation| {
                    relocated_original_index(
                        index,
                        relocation.source_index,
                        relocation.destination_index,
                    )
                });
            let base = compact.children(id).count();
            if source_index >= base {
                return false;
            }
            let Some(child) = compact.children(id).nth(source_index) else {
                return false;
            };
            id = child;
        }
        current.indexes_mut().push(index);
    }
    copied
}

fn overlay_materialized_element_mut<'a>(
    compact: &XmlCompactDocument,
    edits: &'a mut SparseOverlay,
    path: &XmlPath,
) -> Option<&'a mut XmlElement> {
    overlay_materialized_node_mut(compact, edits, path)?.as_element_mut()
}

fn overlay_materialized_node_mut<'a>(
    compact: &XmlCompactDocument,
    edits: &'a mut SparseOverlay,
    path: &XmlPath,
) -> Option<&'a mut XmlNode> {
    let (location, remaining) = overlay_materialized_location(compact, edits, path)?;
    let mut node = match location {
        OverlayMaterializedLocation::Appended { parent, index } => {
            edits.appended.get_mut(&parent)?.get_mut(index)?
        }
        OverlayMaterializedLocation::Ordered { parent, index } => {
            match edits.child_orders.get_mut(&parent)?.get_mut(index)? {
                SparseChild::Materialized(node) => node,
                SparseChild::Compact(_) | SparseChild::CompactCopy { .. } => return None,
            }
        }
    };
    for &child in remaining {
        node = node.as_element_mut()?.children.get_mut(child)?;
    }
    Some(node)
}

enum OverlayMaterializedLocation {
    Appended { parent: XmlPath, index: usize },
    Ordered { parent: XmlPath, index: usize },
}

fn overlay_materialized_location<'a>(
    compact: &XmlCompactDocument,
    edits: &SparseOverlay,
    path: &'a XmlPath,
) -> Option<(OverlayMaterializedLocation, &'a [usize])> {
    let mut id = compact.root();
    let mut parent = XmlPath::root();
    for (depth, &index) in path.indexes().iter().enumerate() {
        if let Some(children) = edits.child_orders.get(&parent) {
            match children.get(index)? {
                SparseChild::Compact(child) | SparseChild::CompactCopy { id: child, .. } => {
                    id = *child;
                    parent.indexes_mut().push(index);
                    continue;
                }
                SparseChild::Materialized(_) => {
                    return Some((
                        OverlayMaterializedLocation::Ordered { parent, index },
                        &path.indexes()[depth + 1..],
                    ));
                }
            }
        }
        let source_index = edits
            .relocations
            .iter()
            .find(|relocation| relocation.parent == parent)
            .map_or(index, |relocation| {
                relocated_original_index(
                    index,
                    relocation.source_index,
                    relocation.destination_index,
                )
            });
        let base = compact.children(id).count();
        if source_index >= base {
            let added = edits.appended.get(&parent)?;
            let added_index = source_index - base;
            added.get(added_index)?;
            return Some((
                OverlayMaterializedLocation::Appended {
                    parent,
                    index: added_index,
                },
                &path.indexes()[depth + 1..],
            ));
        }
        id = compact.children(id).nth(source_index)?;
        parent = parent.child(index);
    }
    None
}

fn ensure_child_order(
    compact: &XmlCompactDocument,
    edits: &mut SparseOverlay,
    parent: &XmlPath,
) -> Result<(), XmlDomError> {
    if edits.child_orders.contains_key(parent) {
        return Ok(());
    }
    let id = overlay_compact_node_at(compact, edits, parent).ok_or(XmlDomError::NotElement)?;
    if compact
        .node(id)
        .is_none_or(|node| node.kind() != XmlNodeKind::Element)
    {
        return Err(XmlDomError::NotElement);
    }
    let mut children: Vec<_> = compact.children(id).map(SparseChild::Compact).collect();
    children.extend(
        edits
            .appended
            .remove(parent)
            .unwrap_or_default()
            .into_iter()
            .map(SparseChild::Materialized),
    );
    if let Some(position) = edits
        .relocations
        .iter()
        .position(|relocation| relocation.parent == *parent)
    {
        let relocation = edits.relocations.remove(position);
        let moved = children.remove(relocation.source_index);
        let destination = relocation.destination_index
            - usize::from(relocation.source_index < relocation.destination_index);
        children.insert(destination, moved);
    }
    edits.child_orders.insert(parent.clone(), children);
    Ok(())
}

fn sparse_attribute_name<'a>(
    compact: &'a XmlCompactDocument,
    attribute: &'a SparseAttribute,
) -> Option<&'a str> {
    match attribute {
        SparseAttribute::Compact(index) => compact.attribute_name(*index),
        SparseAttribute::Materialized(attribute) => Some(&attribute.name),
    }
}

fn collect_compact_attributes(
    compact: &XmlCompactDocument,
    id: crate::XmlViewNodeId,
    overlay: Option<(&SparseOverlay, &XmlPath)>,
) -> Result<Vec<(String, String)>, XmlDomError> {
    let record = compact.node(id).ok_or(XmlDomError::InvalidTarget)?;
    if record.kind() != XmlNodeKind::Element {
        return Err(XmlDomError::NotElement);
    }
    let decode = |index| {
        crate::parser::decode_compact_lexeme(
            compact
                .attribute_value(index)
                .expect("compact attribute value"),
            crate::parser::CompactLexemeKind::Attribute,
            compact.xml11,
            compact.config.attribute_whitespace,
        )
        .map_err(XmlDomError::from)
    };
    let mut output = Vec::new();
    if let Some((edits, path)) = overlay {
        if let Some(attributes) = edits.attribute_orders.get(path) {
            output.reserve(attributes.len());
            for attribute in attributes {
                match attribute {
                    SparseAttribute::Compact(index) => {
                        let name = compact
                            .attribute_name(*index)
                            .expect("compact attribute name");
                        let value = edits
                            .attributes
                            .get(&(path.clone(), name.to_owned()))
                            .cloned()
                            .map_or_else(|| decode(*index), Ok)?;
                        output.push((name.to_owned(), value));
                    }
                    SparseAttribute::Materialized(attribute) => {
                        output.push((attribute.name.clone(), attribute.value.clone()));
                    }
                }
            }
            return Ok(output);
        }
    }
    output.reserve(record.attribute_range().len());
    for index in record.attribute_range() {
        let name = compact
            .attribute_name(index)
            .expect("compact attribute name");
        let value = overlay
            .and_then(|(edits, path)| {
                edits
                    .attributes
                    .get(&(path.clone(), name.to_owned()))
                    .cloned()
            })
            .map_or_else(|| decode(index), Ok)?;
        output.push((name.to_owned(), value));
    }
    if let Some((edits, path)) = overlay {
        let existing: HashSet<String> = output.iter().map(|(name, _)| name.clone()).collect();
        if let Some(order) = edits.added_attribute_order.get(path) {
            for name in order {
                if existing.contains(name) {
                    continue;
                }
                if let Some(value) = edits.attributes.get(&(path.clone(), name.clone())) {
                    output.push((name.clone(), value.clone()));
                }
            }
        }
        let mut added: Vec<_> = edits
            .attributes
            .iter()
            .filter(|((target, name), _)| {
                target == path
                    && !existing.contains(name)
                    && !edits
                        .added_attribute_order
                        .get(path)
                        .is_some_and(|order| order.contains(name))
            })
            .collect();
        added.sort_unstable_by(|left, right| left.0 .1.cmp(&right.0 .1));
        output.extend(
            added
                .into_iter()
                .map(|((_, name), value)| (name.clone(), value.clone())),
        );
    }
    Ok(output)
}

fn ensure_attribute_order(
    compact: &XmlCompactDocument,
    edits: &mut SparseOverlay,
    path: &XmlPath,
) -> Result<(), XmlDomError> {
    if edits.attribute_orders.contains_key(path) {
        return Ok(());
    }
    let id = overlay_compact_node_at(compact, edits, path).ok_or(XmlDomError::NotElement)?;
    let record = compact.node(id).ok_or(XmlDomError::NotElement)?;
    if record.kind() != XmlNodeKind::Element {
        return Err(XmlDomError::NotElement);
    }
    let mut attributes: Vec<_> = record
        .attribute_range()
        .map(SparseAttribute::Compact)
        .collect();
    let original: HashSet<_> = record
        .attribute_range()
        .filter_map(|index| compact.attribute_name(index))
        .collect();
    let mut added = edits.added_attribute_order.remove(path).unwrap_or_default();
    let mut unordered: Vec<_> = edits
        .attributes
        .keys()
        .filter(|(target, name)| {
            target == path
                && !original.contains(name.as_str())
                && !added.iter().any(|candidate| candidate == name)
        })
        .map(|(_, name)| name.clone())
        .collect();
    unordered.sort_unstable();
    added.extend(unordered);
    for name in added {
        if let Some(value) = edits.attributes.remove(&(path.clone(), name.clone())) {
            attributes.push(SparseAttribute::Materialized(
                crate::XmlAttribute::new_unchecked(name, value),
            ));
        }
    }
    edits.attribute_orders.insert(path.clone(), attributes);
    Ok(())
}

fn insert_overlay_attribute(
    inner: &mut XmlDomInner,
    path: &XmlPath,
    index: usize,
    name: String,
    value: String,
) -> Result<(), XmlDomError> {
    let name = validate_mutation_name(name)?;
    crate::mutation::validate_characters(&value)?;
    match &mut inner.state {
        XmlDomState::Overlay { compact, edits } => {
            if let Some(element) = overlay_materialized_element_mut(compact, edits, path) {
                element
                    .insert_attribute(index, crate::XmlAttribute::new_unchecked(name, value))
                    .map_err(|_| XmlDomError::InvalidTarget)?;
            } else {
                ensure_attribute_order(compact, edits, path)?;
                let attributes = edits.attribute_orders.get_mut(path).unwrap();
                if index > attributes.len() {
                    return Err(XmlDomError::InvalidTarget);
                }
                attributes.insert(
                    index,
                    SparseAttribute::Materialized(crate::XmlAttribute::new_unchecked(name, value)),
                );
            }
            edits.mutations += 1;
        }
        XmlDomState::Compact(_) | XmlDomState::Transition => unreachable!(),
    }
    Ok(())
}

fn rebase_after_child_order_change(
    edits: &mut SparseOverlay,
    parent: &XmlPath,
    old: &[Option<SparseCompactIdentity>],
    identity_positions: &HashMap<usize, usize>,
) {
    let Some(new) = edits.child_orders.get(parent) else {
        return;
    };
    let new_positions: HashMap<SparseCompactIdentity, usize> = new
        .iter()
        .enumerate()
        .filter_map(|(index, child)| match child {
            SparseChild::Compact(id) => Some((SparseCompactIdentity::Original(id.index()), index)),
            SparseChild::CompactCopy { identity, .. } => {
                Some((SparseCompactIdentity::Copy(*identity), index))
            }
            SparseChild::Materialized(_) => None,
        })
        .collect();
    let positions: HashMap<usize, usize> = old
        .iter()
        .enumerate()
        .filter_map(|(old_index, child)| {
            (*child).and_then(|id| {
                new_positions
                    .get(&id)
                    .copied()
                    .map(|new_index| (old_index, new_index))
            })
        })
        .collect();

    let identity_cache = std::mem::take(&mut edits.identity_cache);
    rebase_overlay_paths(edits, parent, &positions);
    edits.identity_cache = identity_cache;
    rebase_identity_cache(&mut edits.identity_cache, parent, identity_positions);
}

fn rebase_overlay_paths(
    edits: &mut SparseOverlay,
    parent: &XmlPath,
    positions: &HashMap<usize, usize>,
) {
    edits.appended = edits
        .appended
        .drain()
        .filter_map(|(path, value)| {
            remap_descendant_path(&path, parent, positions).map(|path| (path, value))
        })
        .collect();
    edits.attributes = edits
        .attributes
        .drain()
        .filter_map(|((path, name), value)| {
            remap_descendant_path(&path, parent, positions).map(|path| ((path, name), value))
        })
        .collect();
    edits.added_attribute_order = edits
        .added_attribute_order
        .drain()
        .filter_map(|(path, value)| {
            remap_descendant_path(&path, parent, positions).map(|path| (path, value))
        })
        .collect();
    edits.attribute_orders = edits
        .attribute_orders
        .drain()
        .filter_map(|(path, value)| {
            remap_descendant_path(&path, parent, positions).map(|path| (path, value))
        })
        .collect();
    edits.names = edits
        .names
        .drain()
        .filter_map(|(path, value)| {
            remap_descendant_path(&path, parent, positions).map(|path| (path, value))
        })
        .collect();
    edits.values = edits
        .values
        .drain()
        .filter_map(|(path, value)| {
            remap_descendant_path(&path, parent, positions).map(|path| (path, value))
        })
        .collect();
    edits.child_orders = edits
        .child_orders
        .drain()
        .filter_map(|(path, value)| {
            remap_descendant_path(&path, parent, positions).map(|path| (path, value))
        })
        .collect();
    edits.relocations = edits
        .relocations
        .drain(..)
        .filter_map(|mut relocation| {
            relocation.parent = remap_descendant_path(&relocation.parent, parent, positions)?;
            Some(relocation)
        })
        .collect();
    rebase_identity_cache(&mut edits.identity_cache, parent, positions);
}

fn rebase_identity_cache(
    cache: &mut IdentityCache,
    parent: &XmlPath,
    positions: &HashMap<usize, usize>,
) {
    cache.by_id = cache
        .by_id
        .drain()
        .filter_map(|(id, path)| {
            remap_descendant_path(&path, parent, positions).map(|path| (id, path))
        })
        .collect();
    cache.by_path = cache
        .by_id
        .iter()
        .map(|(id, path)| (path.clone(), *id))
        .collect();
}

fn relocation_positions(
    child_count: usize,
    source_index: usize,
    destination_index: usize,
) -> HashMap<usize, usize> {
    (0..child_count)
        .map(|old_index| {
            let new_index = if source_index < destination_index {
                if old_index == source_index {
                    destination_index - 1
                } else if old_index > source_index && old_index < destination_index {
                    old_index - 1
                } else {
                    old_index
                }
            } else if source_index > destination_index {
                if old_index == source_index {
                    destination_index
                } else if old_index >= destination_index && old_index < source_index {
                    old_index + 1
                } else {
                    old_index
                }
            } else {
                old_index
            };
            (old_index, new_index)
        })
        .collect()
}

fn insertion_positions(child_count: usize, index: usize) -> HashMap<usize, usize> {
    (0..child_count)
        .map(|old_index| {
            let new_index = old_index + usize::from(old_index >= index);
            (old_index, new_index)
        })
        .collect()
}

fn removal_positions(child_count: usize, index: usize) -> HashMap<usize, usize> {
    (0..child_count)
        .filter(|old_index| *old_index != index)
        .map(|old_index| {
            let new_index = old_index - usize::from(old_index > index);
            (old_index, new_index)
        })
        .collect()
}

fn replacement_positions(child_count: usize, index: usize) -> HashMap<usize, usize> {
    (0..child_count)
        .filter(|old_index| *old_index != index)
        .map(|old_index| (old_index, old_index))
        .collect()
}

fn relocated_original_index(
    logical_index: usize,
    source_index: usize,
    destination_index: usize,
) -> usize {
    if source_index < destination_index {
        let relocated = destination_index - 1;
        if logical_index == relocated {
            source_index
        } else if logical_index >= source_index && logical_index < relocated {
            logical_index + 1
        } else {
            logical_index
        }
    } else if source_index > destination_index {
        if logical_index == destination_index {
            source_index
        } else if logical_index > destination_index && logical_index <= source_index {
            logical_index - 1
        } else {
            logical_index
        }
    } else {
        logical_index
    }
}

fn compact_child_identity(children: &[SparseChild]) -> Vec<Option<SparseCompactIdentity>> {
    children
        .iter()
        .map(|child| match child {
            SparseChild::Compact(id) => Some(SparseCompactIdentity::Original(id.index())),
            SparseChild::CompactCopy { identity, .. } => {
                Some(SparseCompactIdentity::Copy(*identity))
            }
            SparseChild::Materialized(_) => None,
        })
        .collect()
}

fn remap_descendant_path(
    path: &XmlPath,
    parent: &XmlPath,
    positions: &HashMap<usize, usize>,
) -> Option<XmlPath> {
    if path.indexes().len() <= parent.indexes().len()
        || !path.indexes().starts_with(parent.indexes())
    {
        return Some(path.clone());
    }
    let old_index = path.indexes()[parent.indexes().len()];
    let new_index = *positions.get(&old_index)?;
    let mut rebased = path.clone();
    rebased.indexes_mut()[parent.indexes().len()] = new_index;
    Some(rebased)
}

fn adjust_path_after_removal(path: &XmlPath, removed: &XmlPath) -> XmlPath {
    let mut adjusted = path.clone();
    let Some((&removed_index, removed_parent)) = removed.indexes().split_last() else {
        return adjusted;
    };
    if adjusted.indexes().len() > removed_parent.len()
        && adjusted.indexes()[..removed_parent.len()] == *removed_parent
        && adjusted.indexes()[removed_parent.len()] > removed_index
    {
        adjusted.indexes_mut()[removed_parent.len()] -= 1;
    }
    adjusted
}

fn snapshot_node(inner: &XmlDomInner, path: &XmlPath) -> Result<XmlNode, XmlDomError> {
    match &inner.state {
        XmlDomState::Compact(compact) => {
            let id = compact_node_at(compact, path).ok_or(XmlDomError::InvalidTarget)?;
            materialize_compact_node(compact, &SparseOverlay::default(), id, path)
                .map_err(Into::into)
        }
        XmlDomState::Overlay { compact, edits } => {
            materialize_overlay_node(compact, edits, path).map_err(Into::into)
        }
        XmlDomState::Transition => unreachable!(),
    }
}

#[derive(Clone, Copy)]
enum XPathSourceNode<'a> {
    Compact(crate::XmlViewNodeId),
    Materialized(&'a XmlNode),
}

struct XPathBuildFrame<'a> {
    path: XmlPath,
    arena_index: usize,
    children: Vec<XPathSourceNode<'a>>,
    next_child: usize,
}

fn build_xpath_arena<'a>(
    compact: &'a XmlCompactDocument,
    edits: &'a SparseOverlay,
) -> Result<XPathArena<'a>, XmlDomError> {
    if edits.appended.is_empty()
        && edits.child_orders.is_empty()
        && edits.attributes.is_empty()
        && edits.added_attribute_order.is_empty()
        && edits.attribute_orders.is_empty()
        && edits.names.is_empty()
        && edits.values.is_empty()
        && edits.relocations.is_empty()
    {
        return build_unedited_xpath_arena(compact, edits);
    }
    let mut arena = XPathArena::with_capacity(
        compact.stats.nodes + edits.appended.values().map(Vec::len).sum::<usize>(),
        compact.stats.attributes + edits.attributes.len(),
    );
    let root_path = XmlPath::root();
    let root_source = XPathSourceNode::Compact(compact.root());
    let root = push_xpath_source(&mut arena, compact, edits, root_source, &root_path, None, 0)?;
    let mut stack = vec![XPathBuildFrame {
        children: xpath_source_children(compact, edits, root_source, &root_path),
        path: root_path,
        arena_index: root,
        next_child: 0,
    }];

    while let Some(frame) = stack.last_mut() {
        if frame.next_child >= frame.children.len() {
            let element = frame.arena_index;
            stack.pop();
            arena.close_element(element);
            continue;
        }
        let child_index = frame.next_child;
        frame.next_child += 1;
        let child_path = frame.path.child(child_index);
        let child_source = frame.children[child_index];
        let parent = frame.arena_index;
        let child = push_xpath_source(
            &mut arena,
            compact,
            edits,
            child_source,
            &child_path,
            Some(parent),
            child_index,
        )?;
        if xpath_source_is_element(compact, child_source) {
            stack.push(XPathBuildFrame {
                children: xpath_source_children(compact, edits, child_source, &child_path),
                path: child_path,
                arena_index: child,
                next_child: 0,
            });
        }
    }
    Ok(arena)
}

fn build_unedited_xpath_arena<'a>(
    compact: &'a XmlCompactDocument,
    edits: &'a SparseOverlay,
) -> Result<XPathArena<'a>, XmlDomError> {
    let mut arena = XPathArena::with_capacity(compact.stats.nodes, compact.stats.attributes);
    let root_path = XmlPath::root();
    let mut open: Vec<(usize, usize, usize)> = Vec::new();
    for id in compact.node_ids() {
        while open.last().is_some_and(|(end, _, _)| id.index() >= *end) {
            let (_, element, _) = open.pop().expect("open XPath element exists");
            arena.close_element(element);
        }
        let (parent, child_index) = open.last_mut().map_or((None, 0), |(_, parent, next)| {
            let child_index = *next;
            *next += 1;
            (Some(*parent), child_index)
        });
        let index = push_xpath_source(
            &mut arena,
            compact,
            edits,
            XPathSourceNode::Compact(id),
            &root_path,
            parent,
            child_index,
        )?;
        let record = compact.node(id).expect("compact XPath node exists");
        if record.kind() == XmlNodeKind::Element {
            open.push((record.next_subtree(), index, 0));
        }
    }
    while let Some((_, index, _)) = open.pop() {
        arena.close_element(index);
    }
    Ok(arena)
}

fn push_xpath_source<'a>(
    arena: &mut XPathArena<'a>,
    compact: &'a XmlCompactDocument,
    edits: &'a SparseOverlay,
    source: XPathSourceNode<'a>,
    path: &XmlPath,
    parent: Option<usize>,
    child_index: usize,
) -> Result<usize, XmlDomError> {
    let (kind, name, value) = match source {
        XPathSourceNode::Compact(id) => {
            let record = compact.node(id).expect("compact XPath node exists");
            let primary = compact
                .input
                .get(record.name_start as usize..(record.name_start + record.name_len) as usize)
                .expect("validated compact XPath range");
            match record.kind() {
                XmlNodeKind::Element => (
                    XPathArenaNodeKind::Element,
                    edits
                        .names
                        .get(path)
                        .map_or(Cow::Borrowed(primary), |name| Cow::Borrowed(name.as_str())),
                    Cow::Borrowed(""),
                ),
                XmlNodeKind::Text | XmlNodeKind::Cdata => (
                    XPathArenaNodeKind::Text,
                    Cow::Borrowed(""),
                    edits.values.get(path).map_or_else(
                        || {
                            let kind = if record.kind() == XmlNodeKind::Text {
                                crate::parser::CompactLexemeKind::Text
                            } else {
                                crate::parser::CompactLexemeKind::Opaque
                            };
                            decode_xpath_lexeme(compact, primary, kind)
                        },
                        |value| Ok(Cow::Borrowed(value.as_str())),
                    )?,
                ),
                XmlNodeKind::Comment => (
                    XPathArenaNodeKind::Comment,
                    Cow::Borrowed(""),
                    edits.values.get(path).map_or_else(
                        || {
                            decode_xpath_lexeme(
                                compact,
                                primary,
                                crate::parser::CompactLexemeKind::Opaque,
                            )
                        },
                        |value| Ok(Cow::Borrowed(value.as_str())),
                    )?,
                ),
                XmlNodeKind::ProcessingInstruction => {
                    let data = compact
                        .input
                        .get(
                            record.attribute_start as usize
                                ..(record.attribute_start + record.attribute_count) as usize,
                        )
                        .expect("validated compact processing-instruction range");
                    (
                        XPathArenaNodeKind::ProcessingInstruction,
                        edits
                            .names
                            .get(path)
                            .map_or(Cow::Borrowed(primary), |name| Cow::Borrowed(name.as_str())),
                        edits.values.get(path).map_or_else(
                            || {
                                decode_xpath_lexeme(
                                    compact,
                                    data,
                                    crate::parser::CompactLexemeKind::Opaque,
                                )
                            },
                            |value| Ok(Cow::Borrowed(value.as_str())),
                        )?,
                    )
                }
            }
        }
        XPathSourceNode::Materialized(node) => match node {
            XmlNode::Element(element) => (
                XPathArenaNodeKind::Element,
                Cow::Borrowed(element.name.as_str()),
                Cow::Borrowed(""),
            ),
            XmlNode::Text(value) | XmlNode::Cdata(value) => (
                XPathArenaNodeKind::Text,
                Cow::Borrowed(""),
                Cow::Borrowed(value.as_str()),
            ),
            XmlNode::Comment(value) => (
                XPathArenaNodeKind::Comment,
                Cow::Borrowed(""),
                Cow::Borrowed(value.as_str()),
            ),
            XmlNode::ProcessingInstruction(pi) => (
                XPathArenaNodeKind::ProcessingInstruction,
                Cow::Borrowed(pi.target.as_str()),
                Cow::Borrowed(pi.data.as_str()),
            ),
        },
    };
    let index = arena.push_node(kind, name, value, parent, child_index);
    match source {
        XPathSourceNode::Compact(id) if kind == XPathArenaNodeKind::Element => {
            let record = compact.node(id).expect("compact XPath element exists");
            if edits.attributes.is_empty()
                && !edits.attribute_orders.contains_key(path)
                && !edits.added_attribute_order.contains_key(path)
            {
                for attribute in record.attribute_range() {
                    let name = compact
                        .attribute_name(attribute)
                        .expect("compact XPath attribute name");
                    let value = crate::parser::decode_compact_lexeme(
                        compact
                            .attribute_value(attribute)
                            .expect("compact XPath attribute value"),
                        crate::parser::CompactLexemeKind::Attribute,
                        compact.xml11,
                        compact.config.attribute_whitespace,
                    )?;
                    arena.push_attribute(index, Cow::Borrowed(name), Cow::Owned(value));
                }
            } else {
                for (name, value) in collect_compact_attributes(compact, id, Some((edits, path)))? {
                    arena.push_attribute(index, Cow::Owned(name), Cow::Owned(value));
                }
            }
        }
        XPathSourceNode::Materialized(XmlNode::Element(element)) => {
            for attribute in &element.attributes {
                arena.push_attribute(
                    index,
                    Cow::Borrowed(&attribute.name),
                    Cow::Borrowed(&attribute.value),
                );
            }
        }
        _ => {}
    }
    Ok(index)
}

fn decode_xpath_lexeme<'a>(
    compact: &XmlCompactDocument,
    value: &'a str,
    kind: crate::parser::CompactLexemeKind,
) -> Result<Cow<'a, str>, XmlDomError> {
    let needs_decode = value.as_bytes().contains(&b'&')
        || value.as_bytes().contains(&b'\r')
        || (compact.xml11 && (value.contains('\u{85}') || value.contains('\u{2028}')));
    if !needs_decode {
        return Ok(Cow::Borrowed(value));
    }
    crate::parser::decode_compact_lexeme(
        value,
        kind,
        compact.xml11,
        compact.config.attribute_whitespace,
    )
    .map(Cow::Owned)
    .map_err(Into::into)
}

fn xpath_source_is_element(compact: &XmlCompactDocument, source: XPathSourceNode<'_>) -> bool {
    match source {
        XPathSourceNode::Compact(id) => compact
            .node(id)
            .is_some_and(|node| node.kind() == XmlNodeKind::Element),
        XPathSourceNode::Materialized(node) => matches!(node, XmlNode::Element(_)),
    }
}

fn xpath_source_is_text(compact: &XmlCompactDocument, source: XPathSourceNode<'_>) -> bool {
    match source {
        XPathSourceNode::Compact(id) => compact
            .node(id)
            .is_some_and(|node| matches!(node.kind(), XmlNodeKind::Text | XmlNodeKind::Cdata)),
        XPathSourceNode::Materialized(node) => matches!(node, XmlNode::Text(_) | XmlNode::Cdata(_)),
    }
}

fn xpath_source_children<'a>(
    compact: &XmlCompactDocument,
    edits: &'a SparseOverlay,
    source: XPathSourceNode<'a>,
    path: &XmlPath,
) -> Vec<XPathSourceNode<'a>> {
    match source {
        XPathSourceNode::Compact(id) => {
            if let Some(children) = edits.child_orders.get(path) {
                return children
                    .iter()
                    .map(|child| match child {
                        SparseChild::Compact(id) | SparseChild::CompactCopy { id, .. } => {
                            XPathSourceNode::Compact(*id)
                        }
                        SparseChild::Materialized(node) => XPathSourceNode::Materialized(node),
                    })
                    .collect();
            }
            let mut children: Vec<_> = compact.children(id).map(XPathSourceNode::Compact).collect();
            children.extend(
                edits
                    .appended
                    .get(path)
                    .into_iter()
                    .flatten()
                    .map(XPathSourceNode::Materialized),
            );
            if let Some(relocation) = edits
                .relocations
                .iter()
                .find(|relocation| relocation.parent == *path)
            {
                let moved = children.remove(relocation.source_index);
                let destination = relocation.destination_index
                    - usize::from(relocation.source_index < relocation.destination_index);
                children.insert(destination, moved);
            }
            children
        }
        XPathSourceNode::Materialized(node) => node.as_element().map_or_else(Vec::new, |element| {
            element
                .children
                .iter()
                .map(XPathSourceNode::Materialized)
                .collect()
        }),
    }
}

fn materialize_overlay_node(
    compact: &XmlCompactDocument,
    edits: &SparseOverlay,
    path: &XmlPath,
) -> Result<XmlNode, XmlError> {
    match overlay_node_at(compact, edits, path).expect("validated overlay path") {
        OverlayNodeRef::Compact(id) => materialize_compact_node(compact, edits, id, path),
        OverlayNodeRef::Materialized(node) => Ok(node.clone()),
    }
}

fn materialize_compact_node(
    compact: &XmlCompactDocument,
    edits: &SparseOverlay,
    id: crate::XmlViewNodeId,
    path: &XmlPath,
) -> Result<XmlNode, XmlError> {
    let record = compact.node(id).expect("compact node identifier");
    let raw_primary = || {
        compact
            .input
            .get(record.name_start as usize..(record.name_start + record.name_len) as usize)
            .expect("validated compact range")
    };
    let decode = |value, kind| {
        crate::parser::decode_compact_lexeme(
            value,
            kind,
            compact.xml11,
            compact.config.attribute_whitespace,
        )
    };
    let edited_value = edits.values.get(path);
    match record.kind() {
        XmlNodeKind::Element => Ok(XmlNode::Element(materialize_compact_element(
            compact, edits, id, path,
        )?)),
        XmlNodeKind::Text => Ok(XmlNode::Text(match edited_value {
            Some(value) => value.clone(),
            None => decode(raw_primary(), crate::parser::CompactLexemeKind::Text)?,
        })),
        XmlNodeKind::Comment => Ok(XmlNode::Comment(match edited_value {
            Some(value) => value.clone(),
            None => decode(raw_primary(), crate::parser::CompactLexemeKind::Opaque)?,
        })),
        XmlNodeKind::Cdata => Ok(XmlNode::Cdata(match edited_value {
            Some(value) => value.clone(),
            None => decode(raw_primary(), crate::parser::CompactLexemeKind::Opaque)?,
        })),
        XmlNodeKind::ProcessingInstruction => {
            let data = compact
                .input
                .get(
                    record.attribute_start as usize
                        ..(record.attribute_start + record.attribute_count) as usize,
                )
                .expect("validated compact PI range");
            Ok(XmlNode::ProcessingInstruction(
                crate::XmlProcessingInstruction {
                    target: edits
                        .names
                        .get(path)
                        .cloned()
                        .unwrap_or_else(|| raw_primary().to_owned()),
                    data: match edited_value {
                        Some(value) => value.clone(),
                        None => decode(data, crate::parser::CompactLexemeKind::Opaque)?,
                    },
                },
            ))
        }
    }
}

fn materialize_compact_element(
    compact: &XmlCompactDocument,
    edits: &SparseOverlay,
    id: crate::XmlViewNodeId,
    path: &XmlPath,
) -> Result<XmlElement, XmlError> {
    let record = compact.node(id).expect("compact element identifier");
    let mut element = XmlElement::new_unchecked(
        edits
            .names
            .get(path)
            .map(String::as_str)
            .unwrap_or_else(|| compact.node_name(id).expect("compact element name")),
    );
    if let Some(order) = edits.attribute_orders.get(path) {
        for attribute in order {
            match attribute {
                SparseAttribute::Compact(index) => {
                    let name = compact
                        .attribute_name(*index)
                        .expect("compact attribute name");
                    let value = if let Some(value) =
                        edits.attributes.get(&(path.clone(), name.to_owned()))
                    {
                        value.clone()
                    } else {
                        crate::parser::decode_compact_lexeme(
                            compact
                                .attribute_value(*index)
                                .expect("compact attribute value"),
                            crate::parser::CompactLexemeKind::Attribute,
                            compact.xml11,
                            compact.config.attribute_whitespace,
                        )?
                    };
                    element
                        .attributes
                        .push(crate::XmlAttribute::new_unchecked(name, value));
                }
                SparseAttribute::Materialized(attribute) => {
                    element.attributes.push(attribute.clone())
                }
            }
        }
    } else {
        for index in record.attribute_range() {
            let name = compact
                .attribute_name(index)
                .expect("compact attribute name");
            let value = if let Some(value) = edits.attributes.get(&(path.clone(), name.to_owned()))
            {
                value.clone()
            } else {
                crate::parser::decode_compact_lexeme(
                    compact
                        .attribute_value(index)
                        .expect("compact attribute value"),
                    crate::parser::CompactLexemeKind::Attribute,
                    compact.xml11,
                    compact.config.attribute_whitespace,
                )?
            };
            element
                .attributes
                .push(crate::XmlAttribute::new_unchecked(name, value));
        }
        let existing: HashSet<String> = element
            .attributes
            .iter()
            .map(|attribute| attribute.name.clone())
            .collect();
        if let Some(order) = edits.added_attribute_order.get(path) {
            for name in order {
                if existing.contains(name) {
                    continue;
                }
                if let Some(value) = edits.attributes.get(&(path.clone(), name.clone())) {
                    element
                        .attributes
                        .push(crate::XmlAttribute::new_unchecked(name, value));
                }
            }
        }
        let mut added: Vec<_> = edits
            .attributes
            .iter()
            .filter(|((target, name), _)| {
                target == path
                    && !existing.contains(name)
                    && !edits
                        .added_attribute_order
                        .get(path)
                        .is_some_and(|order| order.contains(name))
            })
            .collect();
        added.sort_unstable_by(|left, right| left.0 .1.cmp(&right.0 .1));
        for ((_, name), value) in added {
            element
                .attributes
                .push(crate::XmlAttribute::new_unchecked(name, value));
        }
    }

    if let Some(order) = edits.child_orders.get(path) {
        for (index, child) in order.iter().enumerate() {
            let child_path = path.child(index);
            match child {
                SparseChild::Compact(child_id) | SparseChild::CompactCopy { id: child_id, .. } => {
                    let node = materialize_compact_node(compact, edits, *child_id, &child_path)?;
                    element.children.push(node);
                }
                SparseChild::Materialized(node) => element.children.push(node.clone()),
            }
        }
    } else if let Some(relocation) = edits
        .relocations
        .iter()
        .find(|relocation| relocation.parent == *path)
    {
        let mut children: Vec<_> = compact.children(id).map(SparseChild::Compact).collect();
        children.extend(
            edits
                .appended
                .get(path)
                .into_iter()
                .flatten()
                .cloned()
                .map(SparseChild::Materialized),
        );
        let moved = children.remove(relocation.source_index);
        let destination = relocation.destination_index
            - usize::from(relocation.source_index < relocation.destination_index);
        children.insert(destination, moved);
        for (index, child) in children.iter().enumerate() {
            match child {
                SparseChild::Compact(child_id) | SparseChild::CompactCopy { id: child_id, .. } => {
                    let node =
                        materialize_compact_node(compact, edits, *child_id, &path.child(index))?;
                    element.children.push(node);
                }
                SparseChild::Materialized(node) => element.children.push(node.clone()),
            }
        }
    } else {
        for (index, child_id) in compact.children(id).enumerate() {
            let child_path = path.child(index);
            let node = materialize_compact_node(compact, edits, child_id, &child_path)?;
            element.children.push(node);
        }
        element
            .children
            .extend(edits.appended.get(path).into_iter().flatten().cloned());
    }
    Ok(element)
}

fn set_materialized_node_value(node: &mut XmlNode, value: String) -> Result<(), XmlDomError> {
    match node {
        XmlNode::Text(current) | XmlNode::Comment(current) | XmlNode::Cdata(current) => {
            *current = value;
            Ok(())
        }
        XmlNode::ProcessingInstruction(pi) => {
            pi.data = value;
            Ok(())
        }
        XmlNode::Element(_) => Err(XmlDomError::InvalidTarget),
    }
}

fn arena_element_paths(
    arena: &XPathArena<'_>,
    selected: Vec<XPathArenaSelection>,
) -> Result<Vec<XmlPath>, XmlDomError> {
    selected
        .into_iter()
        .filter_map(|selection| match selection {
            XPathArenaSelection::Element(index) => Some(
                arena
                    .element_path(index)
                    .map(XmlPath::from_indexes)
                    .ok_or(XmlDomError::InvalidTarget),
            ),
            XPathArenaSelection::Attribute { .. }
            | XPathArenaSelection::Text(_)
            | XPathArenaSelection::Comment(_)
            | XPathArenaSelection::ProcessingInstruction(_)
            | XPathArenaSelection::Namespace { .. } => None,
        })
        .collect()
}

fn arena_xpath_nodes(
    arena: &XPathArena<'_>,
    selected: Vec<XPathArenaSelection>,
    inner: &Rc<RefCell<XmlDomInner>>,
    generation: u64,
) -> Result<Vec<XmlDomXPathNode>, XmlDomError> {
    let handle = |indexes| {
        new_node_handle(inner, XmlPath::from_indexes(indexes), Some(generation))
            .expect("XPath arena path resolves to a logical node")
    };
    selected
        .into_iter()
        .map(|selection| match selection {
            XPathArenaSelection::Element(index) => arena
                .element_path(index)
                .map(|path| XmlDomXPathNode::Element(handle(path)))
                .ok_or(XmlDomError::InvalidTarget),
            XPathArenaSelection::Attribute { owner, name } => arena
                .element_path(owner)
                .map(|path| XmlDomXPathNode::Attribute {
                    owner: handle(path),
                    name,
                })
                .ok_or(XmlDomError::InvalidTarget),
            XPathArenaSelection::Text(index) => arena
                .node_path(index)
                .map(|path| XmlDomXPathNode::Text(handle(path)))
                .ok_or(XmlDomError::InvalidTarget),
            XPathArenaSelection::Comment(index) => arena
                .node_path(index)
                .map(|path| XmlDomXPathNode::Comment(handle(path)))
                .ok_or(XmlDomError::InvalidTarget),
            XPathArenaSelection::ProcessingInstruction(index) => arena
                .node_path(index)
                .map(|path| XmlDomXPathNode::ProcessingInstruction(handle(path)))
                .ok_or(XmlDomError::InvalidTarget),
            XPathArenaSelection::Namespace { owner, prefix, uri } => arena
                .element_path(owner)
                .map(|path| XmlDomXPathNode::Namespace {
                    owner: handle(path),
                    prefix,
                    uri,
                })
                .ok_or(XmlDomError::InvalidTarget),
        })
        .collect()
}

fn arena_evaluated_xpath_nodes(
    arena: &XPathArena<'_>,
    value: XPathArenaValue,
    inner: &Rc<RefCell<XmlDomInner>>,
    generation: u64,
) -> Result<Vec<XmlDomXPathNode>, XmlDomError> {
    let XPathArenaValue::Nodes(selected) = value else {
        return Err(XmlDomError::XPath(XPathError {
            message: "XPath expression does not evaluate to nodes",
            byte: 0,
        }));
    };
    arena_xpath_nodes(arena, selected, inner, generation)
}

fn arena_evaluated_element_paths(
    arena: &XPathArena<'_>,
    value: XPathArenaValue,
) -> Result<Vec<XmlPath>, XmlDomError> {
    let XPathArenaValue::Nodes(selected) = value else {
        return Err(XmlDomError::XPath(XPathError {
            message: "XPath expression does not evaluate to nodes",
            byte: 0,
        }));
    };
    arena_element_paths(arena, selected)
}

fn walk_compact_stats(document: &XmlCompactDocument) -> XmlTreeStats {
    let mut stats = XmlTreeStats::default();
    let mut checksum = 0usize;
    for id in document.node_ids() {
        let record = document.node(id).expect("compact node id");
        stats.nodes += 1;
        match record.kind() {
            XmlNodeKind::Element => {
                stats.elements += 1;
                checksum = checksum.wrapping_add(record.name_len as usize);
                for index in record.attribute_range() {
                    stats.attributes += 1;
                    let attribute = &document.attributes()[index];
                    checksum = checksum
                        .wrapping_add(attribute.name_len as usize)
                        .wrapping_add(attribute.value_len as usize);
                }
            }
            XmlNodeKind::Text | XmlNodeKind::Cdata => {
                checksum = checksum.wrapping_add(record.name_len as usize);
            }
            XmlNodeKind::ProcessingInstruction => {
                checksum = checksum.wrapping_add(record.name_len as usize);
            }
            XmlNodeKind::Comment => {}
        }
    }
    std::hint::black_box(checksum);
    stats
}

fn walk_overlay_stats(compact: &XmlCompactDocument, edits: &SparseOverlay) -> XmlTreeStats {
    let mut stats = walk_compact_stats(compact);
    let mut checksum = 0usize;
    let mut compact_occurrences: HashMap<usize, isize> = HashMap::new();
    for (path, children) in &edits.child_orders {
        if let Some(parent) = overlay_compact_node_at(compact, edits, path) {
            for child in compact.children(parent) {
                *compact_occurrences.entry(child.index()).or_default() -= 1;
            }
        }
        for child in children {
            match child {
                SparseChild::Compact(child) | SparseChild::CompactCopy { id: child, .. } => {
                    *compact_occurrences.entry(child.index()).or_default() += 1;
                }
                SparseChild::Materialized(child) => {
                    count_materialized_node(child, &mut stats, &mut checksum)
                }
            }
        }
    }
    for nodes in edits.appended.values() {
        for node in nodes {
            count_materialized_node(node, &mut stats, &mut checksum);
        }
    }
    for (index, occurrences) in compact_occurrences {
        if occurrences == 0 {
            continue;
        }
        let delta = compact_subtree_stats(compact, crate::XmlViewNodeId(index));
        apply_stats_delta(&mut stats, delta, occurrences);
    }
    for (path, order) in &edits.attribute_orders {
        let original = overlay_compact_node_at(compact, edits, path)
            .and_then(|id| compact.node(id))
            .map_or(0, |node| node.attribute_range().len());
        stats.attributes = stats
            .attributes
            .checked_add_signed(order.len() as isize - original as isize)
            .expect("valid overlay attribute count");
        for attribute in order {
            match attribute {
                SparseAttribute::Compact(index) => {
                    checksum = checksum
                        .wrapping_add(compact.attribute_name(*index).unwrap_or_default().len())
                        .wrapping_add(compact.attribute_value(*index).unwrap_or_default().len());
                }
                SparseAttribute::Materialized(attribute) => {
                    checksum = checksum
                        .wrapping_add(attribute.name.len())
                        .wrapping_add(attribute.value.len());
                }
            }
        }
    }
    for ((path, name), value) in &edits.attributes {
        if edits.attribute_orders.contains_key(path) {
            continue;
        }
        let exists = overlay_compact_node_at(compact, edits, path)
            .and_then(|id| compact.node(id))
            .is_some_and(|node| {
                node.attribute_range()
                    .any(|index| compact.attribute_name(index) == Some(name.as_str()))
            });
        stats.attributes += usize::from(!exists);
        checksum = checksum.wrapping_add(name.len()).wrapping_add(value.len());
    }
    for (path, name) in &edits.names {
        checksum = checksum
            .wrapping_add(path.indexes().len())
            .wrapping_add(name.len());
    }
    for (path, value) in &edits.values {
        checksum = checksum
            .wrapping_add(path.indexes().len())
            .wrapping_add(value.len());
    }
    std::hint::black_box(checksum);
    stats
}

fn compact_subtree_stats(compact: &XmlCompactDocument, root: crate::XmlViewNodeId) -> XmlTreeStats {
    let end = compact
        .node(root)
        .expect("compact subtree root exists")
        .next_subtree();
    let mut stats = XmlTreeStats::default();
    for index in root.index()..end {
        let node = compact
            .node(crate::XmlViewNodeId(index))
            .expect("compact subtree node exists");
        stats.nodes += 1;
        if node.kind() == XmlNodeKind::Element {
            stats.elements += 1;
            stats.attributes += node.attribute_range().len();
        }
    }
    stats
}

fn apply_stats_delta(stats: &mut XmlTreeStats, delta: XmlTreeStats, multiplier: isize) {
    stats.elements = stats
        .elements
        .checked_add_signed(delta.elements as isize * multiplier)
        .expect("valid overlay element count");
    stats.attributes = stats
        .attributes
        .checked_add_signed(delta.attributes as isize * multiplier)
        .expect("valid overlay attribute count");
    stats.nodes = stats
        .nodes
        .checked_add_signed(delta.nodes as isize * multiplier)
        .expect("valid overlay node count");
}

#[inline(never)]
fn count_materialized_node(node: &XmlNode, stats: &mut XmlTreeStats, checksum: &mut usize) {
    let mut pending = Vec::new();
    let mut current = Some(node);
    while let Some(node) = current {
        current = None;
        stats.nodes += 1;
        match node {
            XmlNode::Element(element) => {
                stats.elements += 1;
                stats.attributes += element.attributes.len();
                *checksum = checksum.wrapping_add(element.name.len());
                for attribute in &element.attributes {
                    *checksum = checksum
                        .wrapping_add(attribute.name.len())
                        .wrapping_add(attribute.value.len());
                }
                let mut children = element.children.iter();
                current = children.next();
                pending.extend(children.rev());
            }
            XmlNode::Text(value) | XmlNode::Comment(value) | XmlNode::Cdata(value) => {
                *checksum = checksum.wrapping_add(value.len());
            }
            XmlNode::ProcessingInstruction(pi) => {
                *checksum = checksum
                    .wrapping_add(pi.target.len())
                    .wrapping_add(pi.data.len());
            }
        }
        if current.is_none() {
            current = pending.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, fmt, mem::size_of};

    use super::{
        next_document_id, CompactTopologyEntry, IdentityCache, SparseChild, SparseOverlay, XmlDom,
        XmlDomError, XmlDomNode, XmlDomNodeSet, XmlDomNodeSetStorage, XmlDomPath, XmlDomState,
    };
    use crate::{
        XPathContext, XPathExpression, XPathVariables, XmlCompactNode, XmlNode, XmlOutputEncoding,
        XmlProcessingInstruction, XmlSerializeOptions, XmlViewNodeId,
    };

    fn assert_xml_equivalent(actual: &str, expected: &str) {
        let actual = XmlDom::parse(actual).unwrap().to_xml_string().unwrap();
        let expected = XmlDom::parse(expected).unwrap().to_xml_string().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn facade_representation_sizes_stay_bounded() {
        eprintln!("XmlDom={}", size_of::<XmlDom>());
        eprintln!("XmlDomNode={}", size_of::<XmlDomNode>());
        eprintln!("XmlDomNodeSet={}", size_of::<XmlDomNodeSet>());
        eprintln!("XmlDomNodeSetStorage={}", size_of::<XmlDomNodeSetStorage>());
        eprintln!("CompactTopologyEntry={}", size_of::<CompactTopologyEntry>());
        eprintln!("XmlCompactNode={}", size_of::<XmlCompactNode<'static>>());
        eprintln!("XmlViewNodeId={}", size_of::<XmlViewNodeId>());
        eprintln!("IdentityCache={}", size_of::<IdentityCache>());
        eprintln!("SparseOverlay={}", size_of::<SparseOverlay>());
        assert!(size_of::<XmlDom>() <= 16);
        assert!(size_of::<XmlDomNode>() <= 80);
        assert!(size_of::<XmlDomNodeSet>() <= 128);
        assert!(size_of::<CompactTopologyEntry>() <= 16);
        assert!(size_of::<XmlCompactNode<'static>>() <= 16);
        assert!(size_of::<XmlViewNodeId>() <= 8);
        assert!(size_of::<IdentityCache>() <= 96);
    }

    #[test]
    fn document_identity_blocks_remain_unique_across_threads() {
        let threads: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| (0..300).map(|_| next_document_id()).collect::<Vec<_>>())
            })
            .collect();
        let identities: Vec<_> = threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(
            identities.iter().copied().collect::<HashSet<_>>().len(),
            identities.len()
        );
    }

    #[test]
    fn sparse_identity_table_allocation_stays_proportional_to_materialized_nodes() {
        let document = XmlDom::parse("<r/>").unwrap();
        let inserted = document.root().append_element("inserted").unwrap();
        let _ = inserted.id();
        let inner = document.inner.borrow();
        let XmlDomState::Overlay { edits, .. } = &inner.state else {
            panic!("insertion did not create the sparse overlay")
        };
        eprintln!(
            "identity_by_id_capacity={} identity_by_path_capacity={} entry_payload_bytes={}",
            edits.identity_cache.by_id.capacity(),
            edits.identity_cache.by_path.capacity(),
            size_of::<(u64, crate::XmlPath)>() + size_of::<(crate::XmlPath, u64)>()
        );
        assert_eq!(edits.identity_cache.by_id.len(), 1);
        assert_eq!(edits.identity_cache.by_path.len(), 1);
        assert!(edits.identity_cache.by_id.capacity() <= 4);
        assert!(edits.identity_cache.by_path.capacity() <= 4);
    }

    #[test]
    fn node_kind_snapshot_and_subtree_output_cover_compact_and_overlay_nodes() {
        use crate::XmlNodeKind;

        let document = XmlDom::parse(
            "<?xml version='1.0'?><r><item xmlns='urn:r' id='1'>text<![CDATA[cdata]]><!--note--><?go now?></item><tail/></r>",
        )
        .unwrap();
        let item = document.root().child("item").unwrap().unwrap();
        let children: Vec<_> = item.children().unwrap().collect();
        assert_eq!(item.kind().unwrap(), XmlNodeKind::Element);
        assert_eq!(children[0].kind().unwrap(), XmlNodeKind::Text);
        assert_eq!(children[1].kind().unwrap(), XmlNodeKind::Cdata);
        assert_eq!(children[2].kind().unwrap(), XmlNodeKind::Comment);
        assert_eq!(
            children[3].kind().unwrap(),
            XmlNodeKind::ProcessingInstruction
        );

        let subtree = item.to_xml_string().unwrap();
        assert!(!subtree.contains("<?xml"));
        assert_xml_equivalent(
            &format!("<wrapper>{subtree}</wrapper>"),
            "<wrapper><item xmlns='urn:r' id='1'>text<![CDATA[cdata]]><!--note--><?go now?></item></wrapper>",
        );
        assert_eq!(
            item.to_inner_xml_string().unwrap(),
            "text<![CDATA[cdata]]><!--note--><?go now?>"
        );

        let snapshot = item.snapshot().unwrap();
        let XmlNode::Element(snapshot) = snapshot else {
            panic!("element snapshot changed kind")
        };
        assert_eq!(snapshot.name, "item");
        assert_eq!(snapshot.attribute("id").unwrap().value, "1");

        children[0].set_value("edited & text").unwrap();
        let appended = item.append_node(XmlNode::Cdata("overlay".into())).unwrap();
        assert_eq!(appended.kind().unwrap(), XmlNodeKind::Cdata);
        assert_eq!(
            item.to_inner_xml_string().unwrap(),
            "edited &amp; text<![CDATA[cdata]]><!--note--><?go now?><![CDATA[overlay]]>"
        );
        assert_eq!(children[2].to_xml_string().unwrap(), "<!--note-->");
    }

    #[test]
    fn subtree_output_ignores_document_only_declaration_and_bom_options() {
        let document = XmlDom::parse("<r><item quote='yes'>A&amp;B</item></r>").unwrap();
        let item = document.root().child("item").unwrap().unwrap();
        let options = XmlSerializeOptions {
            declaration: crate::XmlDeclarationMode::Always,
            write_bom: true,
            quote_style: crate::XmlQuoteStyle::Single,
            ..XmlSerializeOptions::default()
        };
        let output = item.to_xml_string_with_options(&options).unwrap();
        assert_eq!(output, "<item quote='yes'>A&amp;B</item>");
        assert!(!output.starts_with('\u{feff}'));

        let text = item.first_child().unwrap().unwrap();
        assert_eq!(text.to_xml_string().unwrap(), "A&amp;B");
        assert!(matches!(
            text.to_inner_xml_string(),
            Err(super::XmlDomOutputError::Dom(XmlDomError::NotElement))
        ));
    }

    #[test]
    fn streaming_builder_escapes_content_and_finishes_compact() {
        let document = XmlDom::build_with_capacity("catalog", 256, |root| {
            root.attribute("label", "A&B")
                .element("item", |item| {
                    item.attribute_typed("id", 7).element("name", |name| {
                        name.text("left < right");
                    });
                })
                .comment("done")
                .processing_instruction("save", "now");
        })
        .unwrap();
        let output = document.to_xml_string().unwrap();
        assert_xml_equivalent(
            &output,
            "<catalog label='A&amp;B'><item id='7'><name>left &lt; right</name></item><!--done--><?save now?></catalog>",
        );
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Compact(_)
        ));

        let error = XmlDom::build("r", |root| {
            root.text("content").attribute("too-late", "1");
        })
        .unwrap_err();
        assert_eq!(error.kind, crate::XmlErrorKind::InvalidDocumentStructure);
    }

    #[test]
    fn streaming_builder_reports_display_failures_without_panicking() {
        struct FailingDisplay;

        impl fmt::Display for FailingDisplay {
            fn fmt(&self, _formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                Err(fmt::Error)
            }
        }

        for error in [
            XmlDom::build("r", |root| {
                root.text_display(FailingDisplay);
            })
            .unwrap_err(),
            XmlDom::build("r", |root| {
                root.attribute_display("bad", FailingDisplay);
            })
            .unwrap_err(),
        ] {
            assert_eq!(error.kind, crate::XmlErrorKind::InvalidDocumentStructure);
        }
    }

    #[test]
    fn removes_and_reorders_nodes_without_materializing() {
        let document = XmlDom::parse("<r><a/><b/></r>").unwrap();
        let root = document.root();
        let appended = root.append_element("temporary").unwrap();
        appended.set_attribute("key", "value").unwrap();
        appended.set_text("discarded").unwrap();
        let removed = appended.remove().unwrap();
        assert_eq!(removed.as_element().unwrap().name, "temporary");
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { edits, .. } if edits.appended.is_empty()
        ));

        let root = document.root();
        let first = root.first_child().unwrap().unwrap();
        let moved = first.move_to(&root, 2).unwrap();
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { edits, .. } if edits.relocations.len() == 1
        ));
        assert_eq!(document.tree_stats().nodes, 3);

        assert_eq!(document.to_xml_string().unwrap(), "<r><b/><a/></r>");
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { edits, .. } if edits.relocations.len() == 1
        ));

        assert_eq!(moved.name().unwrap().as_deref(), Some("a"));
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));
        assert_eq!(document.to_xml_string().unwrap(), "<r><b/><a/></r>");
    }

    #[test]
    fn compact_overlay_serialization_preserves_all_content_semantics() {
        let source = "<?xml version=\"1.0\"?><!--before--><!DOCTYPE r><?setup yes?><r quoted='a&quot;b'>&amp;<!--c--><![CDATA[x<y]]><?pi data?><a/></r><!--after-->";
        let document = XmlDom::parse(source).unwrap();
        document
            .root()
            .set_attribute("added", "<&\"\t\n\r")
            .unwrap();

        let output = document.to_xml_string().unwrap();
        assert_xml_equivalent(
            &output,
            "<?xml version=\"1.0\"?><!--before--><!DOCTYPE r><?setup yes?><r quoted='a&quot;b' added='&lt;&amp;&quot;&#x9;&#xA;&#xD;'>&amp;<!--c--><![CDATA[x<y]]><?pi data?><a/></r><!--after-->",
        );
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));
    }

    #[test]
    fn default_string_serialization_learns_only_verified_source_equivalence() {
        let canonical = XmlDom::parse("<r a=\"x\"><item/></r>").unwrap();
        let before = canonical.inner.borrow();
        let XmlDomState::Compact(compact) = &before.state else {
            panic!("parsed document should stay compact");
        };
        assert_eq!(compact.default_serialization_is_source.get(), None);
        drop(before);
        assert_eq!(canonical.to_xml_string().unwrap(), "<r a=\"x\"><item/></r>");
        let after = canonical.inner.borrow();
        let XmlDomState::Compact(compact) = &after.state else {
            panic!("serialization should stay compact");
        };
        assert_eq!(compact.default_serialization_is_source.get(), Some(true));
        drop(after);
        assert_eq!(canonical.to_xml_string().unwrap(), "<r a=\"x\"><item/></r>");

        let normalized = XmlDom::parse("<r a='x' />").unwrap();
        assert_eq!(normalized.to_xml_string().unwrap(), "<r a=\"x\"/>");
        let inner = normalized.inner.borrow();
        let XmlDomState::Compact(compact) = &inner.state else {
            panic!("serialization should stay compact");
        };
        assert_eq!(compact.default_serialization_is_source.get(), Some(false));
        drop(inner);
        assert_eq!(normalized.to_xml_string().unwrap(), "<r a=\"x\"/>");
    }

    #[test]
    fn full_stats_include_document_level_nodes_without_materializing() {
        let source =
            "<?xml version=\"1.0\"?><!--before--><!DOCTYPE r><?setup yes?><r><a/></r><!--after-->";
        let document = XmlDom::parse(source).unwrap();
        document.root().set_attribute("edited", "yes").unwrap();

        assert_eq!(document.tree_stats().nodes, 2);
        assert_eq!(document.document_stats().nodes, 7);
        assert_eq!(document.document_stats().elements, 2);
        assert_eq!(document.document_stats().attributes, 1);
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));
    }

    #[test]
    fn realistic_structural_edits_rebase_nested_overlays_without_materializing() {
        let document = XmlDom::parse("<r><a><deep/></a><b/>tail</r>").unwrap();
        let root = document.root();
        let a = root.child("a").unwrap().unwrap();
        a.set_attribute("edited", "yes").unwrap();
        a.child("deep")
            .unwrap()
            .unwrap()
            .set_attribute("nested", "kept")
            .unwrap();

        root.prepend_element("first").unwrap();
        let root = document.root();
        let a = root.child("a").unwrap().unwrap();
        assert_eq!(a.attribute("edited").unwrap().as_deref(), Some("yes"));
        assert_eq!(
            a.child("deep")
                .unwrap()
                .unwrap()
                .attribute("nested")
                .unwrap()
                .as_deref(),
            Some("kept")
        );

        let b = root.child("b").unwrap().unwrap();
        b.insert_before(XmlNode::element_unchecked("middle"))
            .unwrap();
        let root = document.root();
        let b = root.child("b").unwrap().unwrap();
        let removed = b.replace(XmlNode::element_unchecked("last")).unwrap();
        assert_eq!(removed.as_element().unwrap().name, "b");

        assert_eq!(
            document.to_xml_string().unwrap(),
            "<r><first/><a edited=\"yes\"><deep nested=\"kept\"/></a><middle/><last/>tail</r>"
        );
        assert_eq!(document.tree_stats().elements, 6);
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));
    }

    #[test]
    fn empty_document_text_copy_remove_and_clear_stay_optimized() {
        let document = XmlDom::new("config").unwrap();
        let root = document.root();
        let item = root.append_element("item").unwrap();
        item.set_attribute("id", "7").unwrap();
        item.set_text("value").unwrap();
        let copied = root.append_copy(&item).unwrap();
        copied.set_attribute("id", "8").unwrap();

        let root = document.root();
        let first = root.first_child().unwrap().unwrap();
        let removed = first.remove().unwrap();
        assert_eq!(
            removed.as_element().unwrap().attribute("id").unwrap().value,
            "7"
        );
        assert_eq!(
            document.to_xml_string().unwrap(),
            "<config><item id=\"8\">value</item></config>"
        );
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));

        let root = document.root();
        root.clear().unwrap();
        assert_eq!(document.to_xml_string().unwrap(), "<config/>");
        assert_eq!(document.tree_stats().nodes, 1);
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));
    }

    #[test]
    fn ordered_attribute_edits_stay_in_the_sparse_overlay() {
        let document = XmlDom::parse("<r a='1' b='2'/>").unwrap();
        let root = document.root();
        assert!(root.remove_attribute("a").unwrap());
        root.prepend_attribute("c", "3").unwrap();
        root.insert_attribute(1, "d", "4").unwrap();
        assert!(root.replace_attribute("b", "e", "5").unwrap());
        root.set_attribute("d", "updated").unwrap();

        assert_eq!(root.attribute("a").unwrap(), None);
        assert_eq!(root.attribute("d").unwrap().as_deref(), Some("updated"));
        assert_eq!(
            root.attributes().unwrap().collect::<Vec<_>>(),
            vec![
                ("c".to_owned(), "3".to_owned()),
                ("d".to_owned(), "updated".to_owned()),
                ("e".to_owned(), "5".to_owned()),
            ]
        );
        assert_eq!(
            document.to_xml_string().unwrap(),
            "<r c=\"3\" d=\"updated\" e=\"5\"/>"
        );
        assert_eq!(document.tree_stats().attributes, 3);
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));

        root.clear_attributes().unwrap();
        assert_eq!(document.to_xml_string().unwrap(), "<r/>");
        assert_eq!(document.tree_stats().attributes, 0);
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));
    }

    #[test]
    fn renames_and_scalar_node_values_stay_in_the_sparse_overlay() {
        let document = XmlDom::parse("<r><a>old</a><!--before--><?go now?></r>").unwrap();
        let root = document.root();
        root.set_name("renamed").unwrap();
        let element = root.first_child().unwrap().unwrap();
        element.set_name("item").unwrap();
        let text = element.first_child().unwrap().unwrap();
        text.set_value("new & value").unwrap();
        assert_eq!(text.value().unwrap().as_deref(), Some("new & value"));
        assert_eq!(element.text().unwrap().as_deref(), Some("new & value"));

        let comment = element.next_sibling().unwrap().unwrap();
        comment.set_value("after").unwrap();
        let pi = comment.next_sibling().unwrap().unwrap();
        pi.set_name("run").unwrap();
        pi.set_value("later").unwrap();

        assert_eq!(
            document.to_xml_string().unwrap(),
            "<renamed><item>new &amp; value</item><!--after--><?run later?></renamed>"
        );
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));
    }

    #[test]
    fn configured_string_and_encoded_writer_output_do_not_promote() {
        let document = XmlDom::parse("<r><a/></r>").unwrap();
        document.root().append_element("b").unwrap();

        let pretty = document
            .to_xml_string_with_options(&XmlSerializeOptions::pretty())
            .unwrap();
        assert_xml_equivalent(&pretty, "<r><a/><b/></r>");
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));

        let options = XmlSerializeOptions {
            encoding: XmlOutputEncoding::Utf16Le,
            write_bom: true,
            ..XmlSerializeOptions::default()
        };
        let mut bytes = Vec::new();
        document
            .write_xml_with_options(&mut bytes, &options)
            .unwrap();
        assert!(bytes.starts_with(&[0xff, 0xfe]));
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));
    }

    #[test]
    fn cross_parent_move_clones_only_the_moved_subtree_and_stays_sparse() {
        let document = XmlDom::parse("<r><left><x/></left><right><y/></right></r>").unwrap();
        let root = document.root();
        let left = root.child("left").unwrap().unwrap();
        left.child("x")
            .unwrap()
            .unwrap()
            .set_attribute("edited", "yes")
            .unwrap();
        let right = root.child("right").unwrap().unwrap();
        let moved = left.move_to(&right, 1).unwrap();

        assert_eq!(moved.name().unwrap().as_deref(), Some("left"));
        assert_eq!(
            document.to_xml_string().unwrap(),
            "<r><right><y/><left><x edited=\"yes\"/></left></right></r>"
        );
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));
    }

    #[test]
    fn subtree_copy_can_import_from_another_compact_document() {
        let source = XmlDom::parse("<source><item key='v'><nested/></item></source>").unwrap();
        let destination = XmlDom::new("destination").unwrap();
        destination
            .root()
            .append_copy(&source.root().child("item").unwrap().unwrap())
            .unwrap();
        assert_eq!(
            destination.to_xml_string().unwrap(),
            "<destination><item key=\"v\"><nested/></item></destination>"
        );
        assert!(matches!(
            &source.inner.borrow().state,
            XmlDomState::Compact(_)
        ));
        assert!(matches!(
            &destination.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));
    }

    #[test]
    fn unedited_same_document_copy_and_cross_parent_move_reuse_compact_subtrees() {
        let document =
            XmlDom::parse("<r><left><payload><deep/></payload></left><right/></r>").unwrap();
        let root = document.root();
        let left = root.child("left").unwrap().unwrap();
        let copied = root.append_copy(&left).unwrap();
        copied.set_name("left-copy").unwrap();
        let payload = left.child("payload").unwrap().unwrap();
        let right = root.child("right").unwrap().unwrap();
        payload.move_to(&right, 0).unwrap();

        assert_eq!(
            document.to_xml_string().unwrap(),
            "<r><left/><right><payload><deep/></payload></right><left-copy><payload><deep/></payload></left-copy></r>"
        );
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { edits, .. }
                if edits.child_orders.values().flatten().all(|child| matches!(
                    child,
                    SparseChild::Compact(_) | SparseChild::CompactCopy { .. }
                ))
        ));
    }

    #[test]
    fn shared_compact_copies_have_stable_logical_identity_and_no_source_position() {
        let document = XmlDom::parse("<r>\n<a><x/></a><b/>\n</r>").unwrap();
        let root = document.root();
        let original = root.child("a").unwrap().unwrap();
        let first = root.append_copy(&original).unwrap();
        first.set_name("first-copy").unwrap();
        assert!(first.source_position().unwrap().is_none());
        assert!(first
            .child("x")
            .unwrap()
            .unwrap()
            .source_position()
            .unwrap()
            .is_none());
        let second = root.append_copy(&original).unwrap();
        second.set_name("second-copy").unwrap();

        document
            .root()
            .prepend_node(XmlNode::Comment("head".to_owned()))
            .unwrap();

        assert_eq!(
            document.to_xml_string().unwrap(),
            "<r><!--head-->\n<a><x/></a><b/>\n<first-copy><x/></first-copy><second-copy><x/></second-copy></r>"
        );
        assert!(document
            .root()
            .child("first-copy")
            .unwrap()
            .unwrap()
            .source_position()
            .unwrap()
            .is_none());
        assert!(document
            .root()
            .child("second-copy")
            .unwrap()
            .unwrap()
            .source_position()
            .unwrap()
            .is_none());
    }

    #[test]
    fn element_text_edits_are_coherent_after_compact_copy() {
        let document = XmlDom::parse("<r><a><x>A</x><x>B</x></a></r>").unwrap();
        let root = document.root();
        let original = root.child("a").unwrap().unwrap();
        let copied = root.append_copy(&original).unwrap();
        copied.set_name("copy").unwrap();

        let original_second = document
            .root()
            .child("a")
            .unwrap()
            .unwrap()
            .children_named("x")
            .unwrap()
            .nth(1)
            .unwrap();
        original_second.set_text("B2").unwrap();

        let walked_text = original_second.first_child().unwrap().unwrap();
        assert_eq!(walked_text.value().unwrap().as_deref(), Some("B2"));
        assert_eq!(
            original_second.select_string("text()").unwrap().as_deref(),
            Some("B2")
        );
        assert_eq!(
            document.to_xml_string().unwrap(),
            "<r><a><x>A</x><x>B2</x></a><copy><x>A</x><x>B</x></copy></r>"
        );
    }

    #[test]
    fn parsed_nodes_retain_line_positions_while_new_nodes_report_none() {
        let document = XmlDom::parse("<r>\n  <item/>\n</r>").unwrap();
        let root = document.root();
        let item = root.child("item").unwrap().unwrap();
        let position = item.source_position().unwrap().unwrap();
        assert_eq!((position.line, position.column), (2, 4));
        assert!(root
            .append_element("new")
            .unwrap()
            .source_position()
            .unwrap()
            .is_none());
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));
    }

    #[test]
    fn declaration_doctype_and_document_misc_stay_in_the_overlay() {
        let document = XmlDom::new("r").unwrap();
        document.set_declaration("version=\"1.0\"").unwrap();
        document.set_doctype_name("r").unwrap();
        document
            .append_before_root(XmlNode::Comment("before".to_owned()))
            .unwrap();
        document
            .append_after_root(XmlNode::ProcessingInstruction(XmlProcessingInstruction {
                target: "after".to_owned(),
                data: "yes".to_owned(),
            }))
            .unwrap();
        assert_eq!(
            document.to_xml_string().unwrap(),
            "<?xml version=\"1.0\"?><!DOCTYPE r><!--before--><r/><?after yes?>"
        );
        assert_eq!(document.document_stats().nodes, 5);
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));

        document.clear_declaration();
        document.clear_doctype();
        assert!(document.declaration().is_none());
        assert!(document.doctype().is_none());
        assert_eq!(document.document_stats().nodes, 3);
    }

    #[test]
    fn document_misc_nodes_support_ordered_inspection_replacement_and_removal() {
        let document =
            XmlDom::parse("<!--a--><!DOCTYPE r><?before yes?><r/><!--c--><?after yes?>").unwrap();

        let before = document.before_root_nodes();
        assert!(matches!(&before[0], XmlNode::Comment(value) if value == "a"));
        assert!(matches!(
            &before[1],
            XmlNode::ProcessingInstruction(pi) if pi.target == "before" && pi.data == "yes"
        ));
        let after = document.after_root_nodes();
        assert!(matches!(&after[0], XmlNode::Comment(value) if value == "c"));
        assert!(matches!(
            &after[1],
            XmlNode::ProcessingInstruction(pi) if pi.target == "after" && pi.data == "yes"
        ));

        assert!(matches!(
            document.remove_before_root(9),
            Err(XmlDomError::Mutation(
                crate::XmlMutationError::IndexOutOfBounds { index: 9, len: 2 }
            ))
        ));
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Compact(_)
        ));

        let replaced = document
            .replace_before_root(1, XmlNode::Comment("replacement".to_owned()))
            .unwrap();
        assert!(matches!(
            replaced,
            XmlNode::ProcessingInstruction(pi) if pi.target == "before"
        ));
        assert!(matches!(
            document.remove_before_root(0).unwrap(),
            XmlNode::Comment(value) if value == "a"
        ));
        let removed = document.remove_after_root(0).unwrap();
        assert!(matches!(removed, XmlNode::Comment(value) if value == "c"));
        assert!(matches!(
            document.replace_after_root(0, XmlNode::Text("invalid".to_owned())),
            Err(XmlDomError::InvalidTarget)
        ));

        assert_eq!(
            document.to_xml_string().unwrap(),
            "<!DOCTYPE r><!--replacement--><r/><?after yes?>"
        );
        assert_eq!(document.before_root_nodes().len(), 1);
        assert_eq!(document.after_root_nodes().len(), 1);
    }

    #[test]
    fn reset_and_copy_preserve_optimized_independent_state() {
        let source = XmlDom::parse("<source><item/></source>").unwrap();
        source
            .root()
            .child("item")
            .unwrap()
            .unwrap()
            .set_attribute("edited", "yes")
            .unwrap();
        let destination = XmlDom::new("old").unwrap();
        let stale = destination.root();
        destination.copy_from(&source).unwrap();
        assert_eq!(stale.name(), Err(XmlDomError::StaleHandle));
        assert_eq!(
            destination.to_xml_string().unwrap(),
            "<source><item edited=\"yes\"/></source>"
        );
        destination.root().set_name("independent").unwrap();
        assert_eq!(source.root().name().unwrap().as_deref(), Some("source"));

        destination.reset("fresh").unwrap();
        assert_eq!(destination.to_xml_string().unwrap(), "<fresh/>");
        assert!(matches!(
            &destination.inner.borrow().state,
            XmlDomState::Compact(_)
        ));
    }

    #[test]
    fn xpath_results_after_sparse_edits_do_not_change_representation() {
        let document = XmlDom::parse("<r><item id='1'/></r>").unwrap();
        let root = document.root();
        root.append_element("item")
            .unwrap()
            .set_attribute("id", "2")
            .unwrap();
        let selected = document.select_elements("//item[@id='2']").unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].attribute("id").unwrap().as_deref(), Some("2"));
        let relative = root.select_elements("item[@id='2']").unwrap();
        assert_eq!(relative.len(), 1);
        assert_eq!(
            root.select_string("item[@id='2']/@id").unwrap().as_deref(),
            Some("2")
        );
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));
    }

    #[test]
    fn compact_descendant_attribute_queries_return_correct_mixed_content_paths() {
        let document =
            XmlDom::parse("<r>text<a/><b><!--before--><a x='1' y='2'/></b><a x='1' y='2'/></r>")
                .unwrap();
        let selected = document.select_elements("//a[@x][@y]").unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!(
            selected[0]
                .parent()
                .unwrap()
                .unwrap()
                .name()
                .unwrap()
                .as_deref(),
            Some("b")
        );
        assert_eq!(
            selected[1]
                .parent()
                .unwrap()
                .unwrap()
                .name()
                .unwrap()
                .as_deref(),
            Some("r")
        );

        let expression = XPathExpression::compile("//a[@x][@y]").unwrap();
        assert_eq!(
            document
                .select_elements_with_variables(&expression, &XPathVariables::default())
                .unwrap()
                .len(),
            2
        );
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Compact(_)
        ));
    }

    #[test]
    fn compact_query_locations_materialize_only_when_semantics_need_a_path() {
        let document =
            XmlDom::parse("<r><group><member name='first'/><member name='second'/></group></r>")
                .unwrap();
        let selected = document.select_elements("//member[@name]").unwrap();
        assert_eq!(selected.len(), 2);
        assert!(selected.materialized.get().is_none());
        assert!(matches!(
            &*selected[0].path.borrow(),
            XmlDomPath::Compact { .. }
        ));
        assert_eq!(selected[0].name().unwrap().as_deref(), Some("member"));
        assert_eq!(
            selected[0].attribute("name").unwrap().as_deref(),
            Some("first")
        );
        selected[0].set_attribute("edited", "yes").unwrap();
        assert_eq!(
            selected[0].attribute("edited").unwrap().as_deref(),
            Some("yes")
        );
        assert_eq!(
            selected[0]
                .parent()
                .unwrap()
                .unwrap()
                .name()
                .unwrap()
                .as_deref(),
            Some("group")
        );

        document.root().set_attribute("overlay", "yes").unwrap();
        assert_eq!(selected[1].name().unwrap().as_deref(), Some("member"));
        assert_eq!(
            selected[1].attribute("name").unwrap().as_deref(),
            Some("second")
        );

        document.root().clear().unwrap();
        assert_eq!(selected[0].name(), Err(XmlDomError::DeletedHandle));
        assert!(matches!(
            selected[1].parent(),
            Err(XmlDomError::DeletedHandle)
        ));
    }

    #[test]
    fn node_identity_survives_structural_edits_and_only_deleted_nodes_fail() {
        let document = XmlDom::parse("<r><a><x/></a><b/><c/></r>").unwrap();
        let root = document.root();
        let a = root.child("a").unwrap().unwrap();
        let x = a.child("x").unwrap().unwrap();
        let b = root.child("b").unwrap().unwrap();
        let c = root.child("c").unwrap().unwrap();
        let b_id = b.id();
        let c_id = c.id();

        root.prepend_element("first").unwrap();
        assert_eq!(b.name().unwrap().as_deref(), Some("b"));
        assert_eq!(b.id(), b_id);
        assert_eq!(c.id(), c_id);

        b.insert_before(XmlNode::Comment("marker".into())).unwrap();
        assert_eq!(b.name().unwrap().as_deref(), Some("b"));
        a.remove().unwrap();
        assert_eq!(a.name(), Err(XmlDomError::DeletedHandle));
        assert_eq!(x.name(), Err(XmlDomError::DeletedHandle));
        assert_eq!(b.name().unwrap().as_deref(), Some("b"));
        assert_eq!(c.name().unwrap().as_deref(), Some("c"));

        b.replace(XmlNode::element_unchecked("replacement"))
            .unwrap();
        assert_eq!(b.name(), Err(XmlDomError::DeletedHandle));
        assert_eq!(c.name().unwrap().as_deref(), Some("c"));

        let materialized = root.append_element("materialized").unwrap();
        materialized.append_element("p").unwrap();
        let q = materialized.append_element("q").unwrap();
        let q_id = q.id();
        materialized.prepend_element("materialized-first").unwrap();
        assert_eq!(q.name().unwrap().as_deref(), Some("q"));
        assert_eq!(q.id(), q_id);
        q.insert_before(XmlNode::Text("text".into())).unwrap();
        assert_eq!(q.name().unwrap().as_deref(), Some("q"));
        q.move_to(&materialized, 0).unwrap();
        assert_eq!(q.name().unwrap().as_deref(), Some("q"));
        assert_eq!(q.id(), q_id);

        root.clear().unwrap();
        assert_eq!(c.name(), Err(XmlDomError::DeletedHandle));
        assert_eq!(q.name(), Err(XmlDomError::DeletedHandle));
    }

    #[test]
    fn moved_and_copied_subtrees_keep_unambiguous_identity() {
        let document = XmlDom::parse("<r><left><m><n/></m></left><right/></r>").unwrap();
        let root = document.root();
        let left = root.child("left").unwrap().unwrap();
        let right = root.child("right").unwrap().unwrap();
        let moved = left.child("m").unwrap().unwrap();
        let descendant = moved.child("n").unwrap().unwrap();
        let moved_id = moved.id();
        let descendant_id = descendant.id();

        let returned = moved.move_to(&right, 0).unwrap();
        assert_eq!(returned.id(), moved_id);
        assert_eq!(moved.id(), moved_id);
        assert_eq!(descendant.id(), descendant_id);
        assert_eq!(descendant.parent().unwrap().unwrap().id(), moved_id);

        let copied = right.append_copy(&moved).unwrap();
        let copied_descendant = copied.child("n").unwrap().unwrap();
        assert_ne!(copied.id(), moved_id);
        assert_ne!(copied_descendant.id(), descendant_id);
        assert_ne!(copied.id(), copied_descendant.id());

        let selected = document.select_elements("//n").unwrap();
        let selected_ids: HashSet<_> = selected.iter().map(XmlDomNode::id).collect();
        assert_eq!(selected_ids.len(), 2);
        right.prepend_element("before").unwrap();
        assert_eq!(selected[0].name().unwrap().as_deref(), Some("n"));
        assert_eq!(selected[1].name().unwrap().as_deref(), Some("n"));
    }

    #[test]
    fn retained_handle_depth_refreshes_and_reports_lifecycle_errors() {
        let document = XmlDom::parse("<r><left><m><n/></m></left><right/></r>").unwrap();
        let root = document.root();
        let left = root.child("left").unwrap().unwrap();
        let moved = left.child("m").unwrap().unwrap();
        let descendant = moved.child("n").unwrap().unwrap();

        assert_eq!(root.depth(), Ok(0));
        assert_eq!(moved.depth(), Ok(2));
        assert_eq!(descendant.depth(), Ok(3));

        moved.move_to(&root, 1).unwrap();
        assert_eq!(moved.depth(), Ok(1));
        assert_eq!(descendant.depth(), Ok(2));

        moved.remove().unwrap();
        assert_eq!(moved.depth(), Err(XmlDomError::DeletedHandle));
        assert_eq!(descendant.depth(), Err(XmlDomError::DeletedHandle));

        let stale = root.child("left").unwrap().unwrap();
        document.reset("new-root").unwrap();
        assert_eq!(stale.depth(), Err(XmlDomError::StaleHandle));
    }

    #[test]
    fn materialized_cross_parent_move_preserves_registered_subtree_identity() {
        let document = XmlDom::parse("<r><left/><right/></r>").unwrap();
        let root = document.root();
        let left = root.child("left").unwrap().unwrap();
        let right = root.child("right").unwrap().unwrap();
        let moved = left.append_element("m").unwrap();
        let descendant = moved.append_element("n").unwrap();
        let moved_id = moved.id();
        let descendant_id = descendant.id();

        let returned = moved.move_to(&right, 0).unwrap();
        assert_eq!(returned.id(), moved_id);
        assert_eq!(moved.id(), moved_id);
        assert_eq!(descendant.id(), descendant_id);
        assert_eq!(descendant.parent().unwrap().unwrap().id(), moved_id);
    }

    #[test]
    fn zero_copy_subtree_copies_receive_disjoint_identity_ranges() {
        let document = XmlDom::parse("<r><source><a><b/></a></source><copies/></r>").unwrap();
        let root = document.root();
        let source = root.child("source").unwrap().unwrap();
        let copies = root.child("copies").unwrap().unwrap();
        let first = copies.append_copy(&source).unwrap();
        let second = copies.append_copy(&source).unwrap();

        let source_a = source.child("a").unwrap().unwrap();
        let first_a = first.child("a").unwrap().unwrap();
        let second_a = second.child("a").unwrap().unwrap();
        let ids = [
            source.id(),
            source_a.id(),
            source_a.child("b").unwrap().unwrap().id(),
            first.id(),
            first_a.id(),
            first_a.child("b").unwrap().unwrap().id(),
            second.id(),
            second_a.id(),
            second_a.child("b").unwrap().unwrap().id(),
        ];
        assert_eq!(ids.into_iter().collect::<HashSet<_>>().len(), ids.len());
    }

    #[test]
    fn retained_duplicate_name_handles_can_be_sorted_and_moved_to_the_end() {
        let document =
            XmlDom::parse("<r><items><item key='3'/><item key='1'/><item key='2'/></items></r>")
                .unwrap();
        let items = document.root().child("items").unwrap().unwrap();
        let mut collected: Vec<_> = items.children_named("item").unwrap().collect();
        let ids: HashSet<_> = collected.iter().map(XmlDomNode::id).collect();
        collected.sort_by_key(|node| node.attribute("key").unwrap());
        for node in &collected {
            let end = items.children().unwrap().count();
            node.move_to(&items, end).unwrap();
        }
        assert_eq!(
            items
                .children_named("item")
                .unwrap()
                .map(|node| node.attribute("key").unwrap().unwrap())
                .collect::<Vec<_>>(),
            ["1", "2", "3"]
        );
        assert_eq!(
            collected.iter().map(XmlDomNode::id).collect::<HashSet<_>>(),
            ids
        );
    }

    #[test]
    fn tolerant_document_parse_returns_a_closed_prefix_and_preserves_strict_errors() {
        let source = "<r><good id='1'>value</good><broken><x></wrong>";
        let strict = XmlDom::parse(source).unwrap_err();
        let outcome = XmlDom::parse_tolerant(source).unwrap();
        assert_eq!(outcome.diagnostic.as_ref(), Some(&strict));
        assert_eq!(strict.byte, source.len());
        assert_eq!(outcome.consumed_bytes, source.find("</wrong>").unwrap());
        assert_eq!(
            outcome.value.to_xml_string().unwrap(),
            "<r><good id=\"1\">value</good><broken><x/></broken></r>"
        );
        assert_eq!(
            outcome
                .value
                .root()
                .select_string("good/@id")
                .unwrap()
                .as_deref(),
            Some("1")
        );
        assert_eq!(XmlDom::parse(source).unwrap_err(), strict);

        let complete = XmlDom::parse_tolerant("<r><a/></r>").unwrap();
        assert!(complete.diagnostic.is_none());
        assert_eq!(complete.consumed_bytes, "<r><a/></r>".len());
        assert!(XmlDom::parse_tolerant("<broken").is_err());
    }

    #[test]
    fn tolerant_parse_keeps_security_limits_hard_and_reports_encoded_offsets() {
        let deeply_nested = format!("{}{}", "<a>".repeat(129), "</a>".repeat(129));
        assert!(matches!(
            XmlDom::parse_tolerant(deeply_nested),
            Err(crate::XmlError {
                kind: crate::XmlErrorKind::DepthLimitExceeded,
                ..
            })
        ));

        let source = "<r>ok<bad></wrong>";
        let mut utf16 = vec![0xff, 0xfe];
        utf16.extend(source.encode_utf16().flat_map(u16::to_le_bytes));
        let outcome = XmlDom::parse_bytes_tolerant(&utf16).unwrap();
        assert_eq!(
            outcome.diagnostic.as_ref().unwrap().byte,
            2 + source.len() * 2
        );
        assert_eq!(
            outcome.consumed_bytes,
            2 + source.find("</wrong>").unwrap() * 2
        );
        assert_eq!(outcome.value.to_xml_string().unwrap(), "<r>ok<bad/></r>");
    }

    #[test]
    fn native_xpath_arena_covers_realistic_axes_namespaces_predicates_and_scalars() {
        let document = XmlDom::parse(
            "<r xmlns:s='urn:s'><group xml:lang='en'><s:item id='1' score='2'>A&amp;B</s:item><s:item id='2' score='4'><![CDATA[C&amp;D]]></s:item><!--note--><?go yes?></group><tail/></r>",
        )
        .unwrap();
        let second = document
            .root()
            .child("group")
            .unwrap()
            .unwrap()
            .children_named("s:item")
            .unwrap()
            .nth(1)
            .unwrap();
        second.set_attribute("score", "5").unwrap();

        let mut context = XPathContext::default();
        context.namespaces.bind("s", "urn:s").unwrap();
        context.variables.insert("minimum", 3.0).unwrap();
        let expression = XPathExpression::compile(
            "/r/group/s:item[number(@score) >= $minimum] | /r/group/s:item[1]",
        )
        .unwrap();
        let selected = document
            .select_elements_with_context(&expression, &context)
            .unwrap();
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].attribute("id").unwrap().as_deref(), Some("1"));
        assert_eq!(selected[1].attribute("id").unwrap().as_deref(), Some("2"));

        let group = document.root().child("group").unwrap().unwrap();
        assert_eq!(
            group
                .select_elements("*[1]/following-sibling::*")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            group.select_string("*[1]/text()").unwrap().as_deref(),
            Some("A&B")
        );
        assert_eq!(
            document
                .evaluate_xpath_number_with_context(
                    &XPathExpression::compile("count(/r/group/s:item)").unwrap(),
                    &context,
                )
                .unwrap(),
            2.0
        );
        assert!(document
            .evaluate_xpath_boolean_with_context(
                &XPathExpression::compile("/r/group/s:item[@score='5']").unwrap(),
                &context,
            )
            .unwrap());
        assert_eq!(
            document
                .evaluate_xpath_string_with_context(
                    &XPathExpression::compile("string(/r/group/s:item[@id='1'])").unwrap(),
                    &context,
                )
                .unwrap(),
            "A&B"
        );
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Overlay { .. }
        ));
    }

    #[test]
    fn namespace_aware_xpath_selects_attributes_after_sparse_relocation() {
        let document = XmlDom::parse(
            "<r><left xmlns:p='urn:left'><p:item p:key='1' plain='yes'><p:child/></p:item></left><right xmlns:p='urn:right'><sink/></right></r>",
        )
        .unwrap();
        let mut context = XPathContext::default();
        context.namespaces.bind("left", "urn:left").unwrap();
        context.namespaces.bind("right", "urn:right").unwrap();

        let left_key = XPathExpression::compile("//@left:key").unwrap();
        let left_key_explicit = XPathExpression::compile("//attribute::left:key").unwrap();
        let right_key = XPathExpression::compile("//@right:key").unwrap();
        let plain = XPathExpression::compile("//@plain").unwrap();
        assert_eq!(
            document
                .select_nodes_with_context(&left_key, &context)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            document
                .select_nodes_with_context(&left_key_explicit, &context)
                .unwrap()
                .len(),
            1
        );
        assert!(document
            .select_nodes_with_context(&right_key, &context)
            .unwrap()
            .is_empty());
        assert_eq!(
            document
                .select_nodes_with_context(&plain, &context)
                .unwrap()
                .len(),
            1
        );

        let root = document.root();
        let left = root.child("left").unwrap().unwrap();
        let right = root.child("right").unwrap().unwrap();
        let item = left.child("p:item").unwrap().unwrap();
        assert_eq!(
            item.expanded_name()
                .unwrap()
                .unwrap()
                .namespace_uri
                .as_deref(),
            Some("urn:left")
        );
        assert_eq!(
            item.attribute_ns(Some("urn:left"), "key")
                .unwrap()
                .as_deref(),
            Some("1")
        );
        assert_eq!(
            item.attribute_ns(None, "plain").unwrap().as_deref(),
            Some("yes")
        );
        assert_eq!(
            left.child_ns(Some("urn:left"), "item")
                .unwrap()
                .unwrap()
                .id(),
            item.id()
        );
        assert!(matches!(
            &document.inner.borrow().state,
            XmlDomState::Compact(_)
        ));
        let moved = item.move_to(&right, 1).unwrap();
        assert_eq!(
            moved
                .expanded_name()
                .unwrap()
                .unwrap()
                .namespace_uri
                .as_deref(),
            Some("urn:right")
        );
        assert_eq!(
            moved
                .attribute_ns(Some("urn:right"), "key")
                .unwrap()
                .as_deref(),
            Some("1")
        );

        assert!(document
            .select_nodes_with_context(&left_key, &context)
            .unwrap()
            .is_empty());
        assert_eq!(
            document
                .select_nodes_with_context(&right_key, &context)
                .unwrap()
                .len(),
            1
        );

        let copied = left.append_copy(&moved).unwrap();
        assert_eq!(
            copied
                .expanded_name()
                .unwrap()
                .unwrap()
                .namespace_uri
                .as_deref(),
            Some("urn:left")
        );
        assert_eq!(
            document
                .select_nodes_with_context(&left_key, &context)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            document
                .select_nodes_with_context(&right_key, &context)
                .unwrap()
                .len(),
            1
        );

        right.set_attribute("xmlns:p", "urn:right2").unwrap();
        context.namespaces.bind("right2", "urn:right2").unwrap();
        let right2_key = XPathExpression::compile("//@right2:key").unwrap();
        assert!(document
            .select_nodes_with_context(&right_key, &context)
            .unwrap()
            .is_empty());
        assert_eq!(
            document
                .select_nodes_with_context(&right2_key, &context)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            moved
                .expanded_name()
                .unwrap()
                .unwrap()
                .namespace_uri
                .as_deref(),
            Some("urn:right2")
        );

        let serialized = document.to_xml_string().unwrap();
        let reparsed = XmlDom::parse(serialized).unwrap();
        assert_eq!(
            reparsed
                .select_nodes_with_context(&left_key, &context)
                .unwrap()
                .len(),
            1
        );
        assert!(reparsed
            .select_nodes_with_context(&right_key, &context)
            .unwrap()
            .is_empty());
        assert_eq!(
            reparsed
                .select_nodes_with_context(&right2_key, &context)
                .unwrap()
                .len(),
            1
        );
    }
}
#[test]
fn mutation_rejects_invalid_xml_before_changing_the_document() {
    let document = XmlDom::parse("<r keep='yes'><child/></r>").unwrap();
    let root = document.root();
    let original = document.to_xml_string().unwrap();

    for error in [
        root.set_name("bad name").unwrap_err(),
        root.append_element("bad name").unwrap_err(),
        root.prepend_element("bad name").unwrap_err(),
        root.ensure_element("bad name").unwrap_err(),
        root.set_attribute("bad name", "value").unwrap_err(),
        root.prepend_attribute("bad name", "value").unwrap_err(),
        root.insert_attribute(0, "bad name", "value").unwrap_err(),
        root.replace_attribute("keep", "bad name", "value")
            .unwrap_err(),
        document.set_doctype_name("bad name").unwrap_err(),
    ] {
        assert!(matches!(
            error,
            XmlDomError::Mutation(crate::XmlMutationError::InvalidName(ref name))
                if name == "bad name"
        ));
        assert_eq!(document.to_xml_string().unwrap(), original);
    }

    let duplicate_attributes = XmlNode::Element(XmlElement {
        name: "added".to_owned(),
        attributes: vec![
            crate::XmlAttribute::new_unchecked("same", "one"),
            crate::XmlAttribute::new_unchecked("same", "two"),
        ],
        children: Vec::new(),
    });
    for error in [
        root.set_text("bad\0text").unwrap_err(),
        root.set_text("bad\u{fffe}text").unwrap_err(),
        root.set_attribute("valid", "bad\0value").unwrap_err(),
        root.set_attribute("valid", "bad\u{ffff}value").unwrap_err(),
        root.append_node(XmlNode::Comment("bad--comment".to_owned()))
            .unwrap_err(),
        root.append_node(XmlNode::ProcessingInstruction(
            crate::XmlProcessingInstruction {
                target: "xml".to_owned(),
                data: "reserved".to_owned(),
            },
        ))
        .unwrap_err(),
        root.append_node(duplicate_attributes).unwrap_err(),
        root.extend_children([
            XmlNode::Text("would otherwise be inserted".to_owned()),
            XmlNode::Comment("bad--comment".to_owned()),
        ])
        .unwrap_err(),
        document.set_declaration("not-a-declaration").unwrap_err(),
        document
            .set_doctype(crate::XmlDoctype {
                name: "r".to_owned(),
                public_id: Some("public".to_owned()),
                system_id: None,
                internal_subset: None,
            })
            .unwrap_err(),
    ] {
        assert!(matches!(error, XmlDomError::Mutation(_)));
        assert_eq!(document.to_xml_string().unwrap(), original);
    }

    root.set_text("valid é 𝄞").unwrap();
    assert_eq!(root.text().unwrap().as_deref(), Some("valid é 𝄞"));
}
