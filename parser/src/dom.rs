use std::cell::Cell;
use std::sync::{
    atomic::{AtomicU32, AtomicUsize, Ordering},
    Mutex, OnceLock,
};

const MAX_RECURSIVE_CLONE_EQ_DEPTH: usize = 64;
const MAX_RECURSIVE_DROP_DEPTH: usize = 64;
const MAX_DEBUG_DEPTH: usize = 64;

thread_local! {
    static RECURSIVE_CLONE_EQ_DEPTH: Cell<usize> = const { Cell::new(0) };
    static RECURSIVE_DEBUG_DEPTH: Cell<usize> = const { Cell::new(0) };
    static RECURSIVE_DROP_DEPTH: Cell<usize> = const { Cell::new(0) };
    static RAW_SOURCE_OWNERS: Cell<(u32, u32)> = const { Cell::new((0, 0)) };
}

static NEXT_RAW_SOURCE_OWNER: AtomicU32 = AtomicU32::new(1);
const RAW_SOURCE_OWNER_BLOCK_SIZE: u32 = 256;

#[derive(Debug, Default)]
pub(crate) struct DefaultSerializationSourceCache(OnceLock<bool>);

impl DefaultSerializationSourceCache {
    pub(crate) fn known(value: bool) -> Self {
        Self(OnceLock::from(value))
    }

    pub(crate) fn get(&self) -> Option<bool> {
        self.0.get().copied()
    }

    pub(crate) fn set(&self, value: bool) {
        let _ = self.0.set(value);
    }
}

impl Clone for DefaultSerializationSourceCache {
    fn clone(&self) -> Self {
        self.get().map_or_else(Self::default, Self::known)
    }
}

impl PartialEq for DefaultSerializationSourceCache {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for DefaultSerializationSourceCache {}

#[derive(Clone, Copy, Default)]
pub(crate) struct RawSourceOwner(u32);

impl std::fmt::Debug for RawSourceOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RawSourceOwner")
    }
}

impl PartialEq for RawSourceOwner {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for RawSourceOwner {}

pub(crate) fn next_raw_source_owner() -> RawSourceOwner {
    RAW_SOURCE_OWNERS.with(|owners| {
        let (next, end) = owners.get();
        if next < end {
            owners.set((next + 1, end));
            return RawSourceOwner(next);
        }

        let start = NEXT_RAW_SOURCE_OWNER
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                (next < u32::MAX).then(|| next.saturating_add(RAW_SOURCE_OWNER_BLOCK_SIZE))
            })
            .expect("raw source identity space exhausted");
        let end = start.saturating_add(RAW_SOURCE_OWNER_BLOCK_SIZE);
        owners.set((start + 1, end));
        RawSourceOwner(start)
    })
}

#[derive(Clone, Copy)]
struct OverflowRawSource {
    pointer: usize,
    length: u32,
    owner: u32,
    references: u32,
}

struct RegisteredRawSourceSlot {
    owner: AtomicU32,
    pointer: AtomicUsize,
    length: AtomicU32,
    references: AtomicU32,
}

impl RegisteredRawSourceSlot {
    const fn new() -> Self {
        Self {
            owner: AtomicU32::new(0),
            pointer: AtomicUsize::new(0),
            length: AtomicU32::new(0),
            references: AtomicU32::new(0),
        }
    }
}

const RAW_SOURCE_SLOT_COUNT: usize = 256;
const _: () = assert!(RAW_SOURCE_SLOT_COUNT.is_power_of_two());
const RESERVED_RAW_SOURCE_OWNER: u32 = u32::MAX;
static REGISTERED_RAW_SOURCE_SLOTS: [RegisteredRawSourceSlot; RAW_SOURCE_SLOT_COUNT] =
    [const { RegisteredRawSourceSlot::new() }; RAW_SOURCE_SLOT_COUNT];
static OVERFLOW_RAW_SOURCES: OnceLock<Mutex<Vec<OverflowRawSource>>> = OnceLock::new();

fn overflow_raw_sources() -> &'static Mutex<Vec<OverflowRawSource>> {
    OVERFLOW_RAW_SOURCES.get_or_init(|| Mutex::new(Vec::with_capacity(8)))
}

fn lock_overflow_raw_sources() -> std::sync::MutexGuard<'static, Vec<OverflowRawSource>> {
    overflow_raw_sources()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn register_raw_source(pointer: usize, length: u32, owner: u32) {
    let start = owner as usize & (RAW_SOURCE_SLOT_COUNT - 1);
    for offset in 0..RAW_SOURCE_SLOT_COUNT {
        let slot = &REGISTERED_RAW_SOURCE_SLOTS[(start + offset) & (RAW_SOURCE_SLOT_COUNT - 1)];
        if slot
            .owner
            .compare_exchange(
                0,
                RESERVED_RAW_SOURCE_OWNER,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            slot.pointer.store(pointer, Ordering::Relaxed);
            slot.length.store(length, Ordering::Relaxed);
            slot.references.store(1, Ordering::Relaxed);
            slot.owner.store(owner, Ordering::Release);
            return;
        }
    }

    lock_overflow_raw_sources().push(OverflowRawSource {
        pointer,
        length,
        owner,
        references: 1,
    });
}

fn retain_raw_source(pointer: usize, length: u32, owner: u32) {
    let start = owner as usize & (RAW_SOURCE_SLOT_COUNT - 1);
    for offset in 0..RAW_SOURCE_SLOT_COUNT {
        let slot = &REGISTERED_RAW_SOURCE_SLOTS[(start + offset) & (RAW_SOURCE_SLOT_COUNT - 1)];
        if slot.owner.load(Ordering::Acquire) == owner
            && slot.pointer.load(Ordering::Relaxed) == pointer
            && slot.length.load(Ordering::Relaxed) == length
            && slot.owner.load(Ordering::Acquire) == owner
        {
            slot.references.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    let mut sources = lock_overflow_raw_sources();
    let source = sources
        .iter_mut()
        .find(|source| {
            source.pointer == pointer && source.length == length && source.owner == owner
        })
        .expect("raw source registration was already removed");
    source.references = source
        .references
        .checked_add(1)
        .expect("raw source registration reference count overflowed");
}

fn unregister_raw_source(pointer: usize, length: u32, owner: u32) {
    let start = owner as usize & (RAW_SOURCE_SLOT_COUNT - 1);
    for offset in 0..RAW_SOURCE_SLOT_COUNT {
        let slot = &REGISTERED_RAW_SOURCE_SLOTS[(start + offset) & (RAW_SOURCE_SLOT_COUNT - 1)];
        if slot.owner.load(Ordering::Acquire) == owner
            && slot.pointer.load(Ordering::Relaxed) == pointer
            && slot.length.load(Ordering::Relaxed) == length
            && slot.owner.load(Ordering::Acquire) == owner
        {
            if slot.references.fetch_sub(1, Ordering::AcqRel) == 1 {
                slot.owner.store(0, Ordering::Release);
            }
            return;
        }
    }

    let mut sources = lock_overflow_raw_sources();
    let Some(index) = sources.iter().position(|source| {
        source.pointer == pointer && source.length == length && source.owner == owner
    }) else {
        debug_assert!(false, "raw source registration was already removed");
        return;
    };
    if sources[index].references > 1 {
        sources[index].references -= 1;
    } else {
        sources.swap_remove(index);
    }
}

#[inline]
fn raw_source_is_registered(input: &str, owner: u32) -> bool {
    let pointer = input.as_ptr() as usize;
    let Ok(length) = u32::try_from(input.len()) else {
        return false;
    };
    let start = owner as usize & (RAW_SOURCE_SLOT_COUNT - 1);
    for offset in 0..RAW_SOURCE_SLOT_COUNT {
        let slot = &REGISTERED_RAW_SOURCE_SLOTS[(start + offset) & (RAW_SOURCE_SLOT_COUNT - 1)];
        if slot.owner.load(Ordering::Acquire) == owner {
            let matches = slot.pointer.load(Ordering::Relaxed) == pointer
                && slot.length.load(Ordering::Relaxed) == length;
            if slot.owner.load(Ordering::Acquire) == owner {
                return matches;
            }
        }
    }
    OVERFLOW_RAW_SOURCES.get().is_some_and(|sources| {
        sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|source| {
                source.pointer == pointer && source.length == length && source.owner == owner
            })
    })
}

pub(crate) struct RawSourceRegistration {
    pointer: usize,
    length_and_owner: u64,
}

impl RawSourceRegistration {
    pub(crate) fn new(input: &str, owner: RawSourceOwner) -> Self {
        let length = u32::try_from(input.len()).expect("parsed compact XML input fits in u32");
        let pointer = input.as_ptr() as usize;
        register_raw_source(pointer, length, owner.0);
        Self {
            pointer,
            length_and_owner: u64::from(length) | (u64::from(owner.0) << 32),
        }
    }

    fn unregistered_alias(owner: RawSourceOwner) -> Self {
        Self {
            pointer: 0,
            length_and_owner: u64::from(owner.0) << 32,
        }
    }

    fn owner(&self) -> RawSourceOwner {
        RawSourceOwner((self.length_and_owner >> 32) as u32)
    }

    fn length(&self) -> u32 {
        self.length_and_owner as u32
    }
}

impl Clone for RawSourceRegistration {
    fn clone(&self) -> Self {
        if self.pointer == 0 {
            return Self::unregistered_alias(self.owner());
        }
        retain_raw_source(self.pointer, self.length(), self.owner().0);
        Self {
            pointer: self.pointer,
            length_and_owner: self.length_and_owner,
        }
    }
}

impl Drop for RawSourceRegistration {
    fn drop(&mut self) {
        if self.pointer != 0 {
            unregister_raw_source(self.pointer, self.length(), self.owner().0);
        }
    }
}

impl std::fmt::Debug for RawSourceRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RawSourceRegistration")
    }
}

impl PartialEq for RawSourceRegistration {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for RawSourceRegistration {}

/// An opaque, lifetime-bound source token for resolving copied raw node and attribute records.
///
/// Obtain this value from [`XmlDocumentView::raw_source`] or
/// [`XmlCompactDocument::raw_source`]. The token prevents a raw record copied from one parse from
/// being resolved against an unrelated string, including allocator-reused storage.
#[derive(Clone, Copy)]
pub struct XmlRawSource<'a> {
    input: &'a str,
    owner: u32,
}

impl std::fmt::Debug for XmlRawSource<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("XmlRawSource")
            .finish_non_exhaustive()
    }
}

/// The semantic kind of a node in any XML tree representation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum XmlNodeKind {
    /// An element node.
    Element,
    /// Parsed character data.
    Text,
    /// A comment node.
    Comment,
    /// A CDATA section.
    Cdata,
    /// A processing instruction.
    ProcessingInstruction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A public `XmlDoctype` value in the XML data model.
pub struct XmlDoctype {
    pub(crate) name: String,
    pub(crate) public_id: Option<String>,
    pub(crate) system_id: Option<String>,
    pub(crate) internal_subset: Option<String>,
}

/// A public `XmlElement` value in the XML data model.
///
/// Debug output preserves the complete derived-style representation for ordinary trees and marks
/// deeply nested child lists as non-exhaustive instead of risking process stack exhaustion.
pub struct XmlElement {
    pub(crate) name: String,
    pub(crate) attributes: Vec<XmlAttribute>,
    pub(crate) children: Vec<XmlNode>,
}

impl Drop for XmlElement {
    fn drop(&mut self) {
        let children = std::mem::take(&mut self.children);
        if let Some(_guard) = RecursiveDropGuard::enter() {
            drop(children);
        } else {
            let mut pending = children;
            drop_pending_xml_nodes(&mut pending);
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A public `XmlAttribute` value in the XML data model.
pub struct XmlAttribute {
    pub(crate) name: String,
    pub(crate) value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A public `XmlProcessingInstruction` value in the XML data model.
pub struct XmlProcessingInstruction {
    pub(crate) target: String,
    pub(crate) data: String,
}

/// Counts for an XML element tree.
///
/// Document-level declarations, doctypes, and misc nodes are excluded unless an API explicitly
/// states that it returns complete-document statistics.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct XmlTreeStats {
    /// The elements.
    pub elements: usize,
    /// The attributes.
    pub attributes: usize,
    /// The nodes.
    pub nodes: usize,
}

/// Controls whether mutation keeps reusable vector capacity or asks the allocator to release it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum XmlMemoryRetention {
    /// Keep spare capacity for subsequent edits (the default).
    #[default]
    RetainCapacity,
    /// Shrink affected vectors after the operation; allocators may still retain memory.
    ReleaseSpareCapacity,
}

/// The supported `XmlNode` alternatives.
pub enum XmlNode {
    /// Indicates `Element`.
    Element(XmlElement),
    /// Indicates `Text`.
    Text(String),
    /// Indicates `Comment`.
    Comment(String),
    /// Indicates `Cdata`.
    Cdata(String),
    /// Indicates `ProcessingInstruction`.
    ProcessingInstruction(XmlProcessingInstruction),
}

impl Clone for XmlElement {
    fn clone(&self) -> Self {
        clone_xml_element_fast(self)
    }
}

impl PartialEq for XmlElement {
    fn eq(&self, other: &Self) -> bool {
        xml_elements_equal_fast(self, other)
    }
}

impl Eq for XmlElement {}

impl Clone for XmlNode {
    fn clone(&self) -> Self {
        clone_xml_node_fast(self)
    }
}

impl PartialEq for XmlNode {
    fn eq(&self, other: &Self) -> bool {
        xml_node_equal_fast(self, other)
    }
}

impl Eq for XmlNode {}

impl std::fmt::Debug for XmlElement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Some(_guard) = RecursiveDebugGuard::enter() else {
            return formatter
                .debug_struct("XmlElement")
                .field("name", &self.name)
                .field("attributes", &self.attributes)
                .field("children", &NonExhaustiveXmlNodes)
                .finish();
        };
        formatter
            .debug_struct("XmlElement")
            .field("name", &self.name)
            .field("attributes", &self.attributes)
            .field("children", &self.children)
            .finish()
    }
}

struct NonExhaustiveXmlNodes;

impl std::fmt::Debug for NonExhaustiveXmlNodes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_list().entry(&DebugTruncated).finish()
    }
}

struct DebugTruncated;

impl std::fmt::Debug for DebugTruncated {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("..")
    }
}

impl std::fmt::Debug for XmlNode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Element(element) => formatter.debug_tuple("Element").field(element).finish(),
            Self::Text(value) => formatter.debug_tuple("Text").field(value).finish(),
            Self::Comment(value) => formatter.debug_tuple("Comment").field(value).finish(),
            Self::Cdata(value) => formatter.debug_tuple("Cdata").field(value).finish(),
            Self::ProcessingInstruction(pi) => formatter
                .debug_tuple("ProcessingInstruction")
                .field(pi)
                .finish(),
        }
    }
}

struct RecursiveDropGuard;

struct RecursiveCloneEqGuard;

struct RecursiveDebugGuard;

impl RecursiveDropGuard {
    fn enter() -> Option<Self> {
        RECURSIVE_DROP_DEPTH.with(|depth| {
            let current = depth.get();
            if current >= MAX_RECURSIVE_DROP_DEPTH {
                None
            } else {
                depth.set(current + 1);
                Some(Self)
            }
        })
    }
}

impl Drop for RecursiveDropGuard {
    fn drop(&mut self) {
        RECURSIVE_DROP_DEPTH.with(|depth| depth.set(depth.get() - 1));
    }
}

impl RecursiveCloneEqGuard {
    fn enter() -> Option<Self> {
        RECURSIVE_CLONE_EQ_DEPTH.with(|depth| {
            let current = depth.get();
            if current >= MAX_RECURSIVE_CLONE_EQ_DEPTH {
                None
            } else {
                depth.set(current + 1);
                Some(Self)
            }
        })
    }
}

impl Drop for RecursiveCloneEqGuard {
    fn drop(&mut self) {
        RECURSIVE_CLONE_EQ_DEPTH.with(|depth| depth.set(depth.get() - 1));
    }
}

impl RecursiveDebugGuard {
    fn enter() -> Option<Self> {
        RECURSIVE_DEBUG_DEPTH.with(|depth| {
            let current = depth.get();
            if current >= MAX_DEBUG_DEPTH {
                None
            } else {
                depth.set(current + 1);
                Some(Self)
            }
        })
    }
}

impl Drop for RecursiveDebugGuard {
    fn drop(&mut self) {
        RECURSIVE_DEBUG_DEPTH.with(|depth| depth.set(depth.get() - 1));
    }
}

fn drop_pending_xml_nodes(pending: &mut Vec<XmlNode>) {
    while let Some(node) = pending.pop() {
        if let XmlNode::Element(mut element) = node {
            pending.append(&mut element.children);
        }
    }
}

fn clone_xml_element_fast(source: &XmlElement) -> XmlElement {
    if let Some(_guard) = RecursiveCloneEqGuard::enter() {
        XmlElement {
            name: source.name.clone(),
            attributes: source.attributes.clone(),
            children: source.children.clone(),
        }
    } else {
        clone_xml_element_iterative(source)
    }
}

fn clone_xml_node_fast(source: &XmlNode) -> XmlNode {
    if let Some(_guard) = RecursiveCloneEqGuard::enter() {
        match source {
            XmlNode::Element(element) => XmlNode::Element(element.clone()),
            XmlNode::Text(value) => XmlNode::Text(value.clone()),
            XmlNode::Comment(value) => XmlNode::Comment(value.clone()),
            XmlNode::Cdata(value) => XmlNode::Cdata(value.clone()),
            XmlNode::ProcessingInstruction(value) => XmlNode::ProcessingInstruction(value.clone()),
        }
    } else {
        match source {
            XmlNode::Element(element) => XmlNode::Element(clone_xml_element_iterative(element)),
            XmlNode::Text(value) => XmlNode::Text(value.clone()),
            XmlNode::Comment(value) => XmlNode::Comment(value.clone()),
            XmlNode::Cdata(value) => XmlNode::Cdata(value.clone()),
            XmlNode::ProcessingInstruction(value) => XmlNode::ProcessingInstruction(value.clone()),
        }
    }
}

fn xml_elements_equal_fast(left: &XmlElement, right: &XmlElement) -> bool {
    if let Some(_guard) = RecursiveCloneEqGuard::enter() {
        left.name == right.name
            && left.attributes == right.attributes
            && left.children == right.children
    } else {
        xml_elements_equal_iterative(left, right)
    }
}

fn xml_node_equal_fast(left: &XmlNode, right: &XmlNode) -> bool {
    if let Some(_guard) = RecursiveCloneEqGuard::enter() {
        match (left, right) {
            (XmlNode::Element(left), XmlNode::Element(right)) => left == right,
            (XmlNode::Text(left), XmlNode::Text(right))
            | (XmlNode::Comment(left), XmlNode::Comment(right))
            | (XmlNode::Cdata(left), XmlNode::Cdata(right)) => left == right,
            (XmlNode::ProcessingInstruction(left), XmlNode::ProcessingInstruction(right)) => {
                left == right
            }
            _ => false,
        }
    } else {
        xml_node_equal_iterative(left, right)
    }
}

fn clone_xml_element_iterative(source: &XmlElement) -> XmlElement {
    let mut stack = vec![XmlElementCloneFrame::new(source)];

    loop {
        let frame = stack
            .last_mut()
            .expect("XML clone stack always starts with root");
        if frame.index < frame.source.children.len() {
            let child = &frame.source.children[frame.index];
            frame.index += 1;
            match child {
                XmlNode::Element(element) => stack.push(XmlElementCloneFrame::new(element)),
                XmlNode::Text(value) => frame.output.children.push(XmlNode::Text(value.clone())),
                XmlNode::Comment(value) => {
                    frame.output.children.push(XmlNode::Comment(value.clone()))
                }
                XmlNode::Cdata(value) => frame.output.children.push(XmlNode::Cdata(value.clone())),
                XmlNode::ProcessingInstruction(value) => frame
                    .output
                    .children
                    .push(XmlNode::ProcessingInstruction(value.clone())),
            }
            continue;
        }

        let completed = stack
            .pop()
            .expect("XML clone stack always contains the active frame")
            .output;
        if let Some(parent) = stack.last_mut() {
            parent.output.children.push(XmlNode::Element(completed));
        } else {
            return completed;
        }
    }
}

struct XmlElementCloneFrame<'a> {
    source: &'a XmlElement,
    output: XmlElement,
    index: usize,
}

impl<'a> XmlElementCloneFrame<'a> {
    fn new(source: &'a XmlElement) -> Self {
        Self {
            source,
            output: XmlElement {
                name: source.name.clone(),
                attributes: source.attributes.clone(),
                children: Vec::with_capacity(source.children.len()),
            },
            index: 0,
        }
    }
}

fn xml_node_equal_iterative(left: &XmlNode, right: &XmlNode) -> bool {
    match (left, right) {
        (XmlNode::Element(left), XmlNode::Element(right)) => {
            xml_elements_equal_iterative(left, right)
        }
        (XmlNode::Text(left), XmlNode::Text(right))
        | (XmlNode::Comment(left), XmlNode::Comment(right))
        | (XmlNode::Cdata(left), XmlNode::Cdata(right)) => left == right,
        (XmlNode::ProcessingInstruction(left), XmlNode::ProcessingInstruction(right)) => {
            left == right
        }
        _ => false,
    }
}

fn xml_elements_equal_iterative(left: &XmlElement, right: &XmlElement) -> bool {
    if left.name != right.name
        || left.attributes != right.attributes
        || left.children.len() != right.children.len()
    {
        return false;
    }
    xml_nodes_equal_iterative(&left.children, &right.children)
}

fn xml_nodes_equal_iterative(left: &[XmlNode], right: &[XmlNode]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut pending_nodes: Vec<_> = left.iter().zip(right.iter()).collect();
    while let Some((left, right)) = pending_nodes.pop() {
        match (left, right) {
            (XmlNode::Element(left), XmlNode::Element(right)) => {
                if left.name != right.name
                    || left.attributes != right.attributes
                    || left.children.len() != right.children.len()
                {
                    return false;
                }
                pending_nodes.extend(left.children.iter().zip(right.children.iter()));
            }
            (XmlNode::Text(left), XmlNode::Text(right))
            | (XmlNode::Comment(left), XmlNode::Comment(right))
            | (XmlNode::Cdata(left), XmlNode::Cdata(right)) => {
                if left != right {
                    return false;
                }
            }
            (XmlNode::ProcessingInstruction(left), XmlNode::ProcessingInstruction(right)) => {
                if left != right {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A public `XmlDocumentView` value in the XML data model.
pub struct XmlDocumentView<'a> {
    pub(crate) input: &'a str,
    pub(crate) root: XmlViewNodeId,
    pub(crate) nodes: Vec<RawXmlNode>,
    pub(crate) attributes: Vec<RawXmlAttribute>,
    pub(crate) stats: XmlTreeStats,
    pub(crate) has_namespace_declarations: bool,
    pub(crate) xml11: bool,
    pub(crate) compact_lexemes_are_borrowable: bool,
    pub(crate) compact_attribute_lexemes_are_borrowable: bool,
    pub(crate) raw_source_registration: RawSourceRegistration,
}

/// A compact XML document.
///
/// The source buffer owns all text while nodes and attributes store stable byte ranges into it.
/// This keeps the document freely movable without self-references and avoids one allocation per
/// name and text node. Use [`crate::XmlDom`] when direct tree mutation is needed.
#[derive(Debug, Eq, PartialEq)]
pub struct XmlCompactDocument {
    pub(crate) input: String,
    pub(crate) root: XmlViewNodeId,
    pub(crate) nodes: Vec<RawXmlNode>,
    pub(crate) attributes: Vec<RawXmlAttribute>,
    pub(crate) stats: XmlTreeStats,
    pub(crate) metadata: CompactDocumentMetadata,
    pub(crate) config: crate::ParserConfig,
    pub(crate) xml11: bool,
    pub(crate) compact_lexemes_are_borrowable: bool,
    pub(crate) compact_attribute_lexemes_are_borrowable: bool,
    pub(crate) default_serialization_is_source: DefaultSerializationSourceCache,
    pub(crate) has_namespace_declarations: bool,
    pub(crate) raw_source_registration: RawSourceRegistration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompactDocumentMetadata {
    pub(crate) declaration: Option<XmlProcessingInstruction>,
    pub(crate) doctype: Option<XmlDoctype>,
    pub(crate) misc_before_root: Vec<XmlNode>,
    pub(crate) misc_after_root: Vec<XmlNode>,
    pub(crate) doctype_before_misc_index: Option<usize>,
}

impl Clone for XmlCompactDocument {
    fn clone(&self) -> Self {
        let input = self.input.clone();
        Self {
            raw_source_registration: RawSourceRegistration::unregistered_alias(
                self.raw_source_registration.owner(),
            ),
            input,
            root: self.root,
            nodes: self.nodes.clone(),
            attributes: self.attributes.clone(),
            stats: self.stats,
            metadata: self.metadata.clone(),
            config: self.config,
            xml11: self.xml11,
            compact_lexemes_are_borrowable: self.compact_lexemes_are_borrowable,
            compact_attribute_lexemes_are_borrowable: self.compact_attribute_lexemes_are_borrowable,
            default_serialization_is_source: self.default_serialization_is_source.clone(),
            has_namespace_declarations: self.has_namespace_declarations,
        }
    }
}

impl XmlCompactDocument {
    pub(crate) fn empty_with_root(root_name: String) -> Result<Self, crate::XmlError> {
        let mut characters = root_name.char_indices();
        let Some((_, first)) = characters.next() else {
            return Err(crate::XmlError::new(crate::XmlErrorKind::InvalidName, 1));
        };
        if !crate::syntax::is_name_start_char(first) {
            return Err(crate::XmlError::new(crate::XmlErrorKind::InvalidName, 1));
        }
        if let Some((byte, _)) =
            characters.find(|(_, character)| !crate::syntax::is_name_char(*character))
        {
            return Err(crate::XmlError::new(
                crate::XmlErrorKind::InvalidName,
                byte + 1,
            ));
        }

        let mut input = String::with_capacity(root_name.len() + 3);
        input.push('<');
        input.push_str(&root_name);
        input.push_str("/>");
        let source_owner = next_raw_source_owner();
        let name_len = u32::try_from(root_name.len())
            .map_err(|_| crate::XmlError::new(crate::XmlErrorKind::InvalidName, 1))?;
        let root = XmlViewNodeId(0);
        let raw_source_registration = RawSourceRegistration::new(&input, source_owner);
        Ok(Self {
            nodes: vec![RawXmlNode::new_with_owner(
                source_owner,
                XmlNodeKind::Element,
                1,
                name_len,
                0,
                0,
                u32::MAX,
                1,
            )],
            input,
            root,
            attributes: Vec::new(),
            stats: XmlTreeStats {
                elements: 1,
                attributes: 0,
                nodes: 1,
            },
            metadata: CompactDocumentMetadata {
                declaration: None,
                doctype: None,
                misc_before_root: Vec::new(),
                misc_after_root: Vec::new(),
                doctype_before_misc_index: None,
            },
            config: crate::ParserConfig::default(),
            xml11: false,
            compact_lexemes_are_borrowable: true,
            compact_attribute_lexemes_are_borrowable: true,
            default_serialization_is_source: DefaultSerializationSourceCache::known(true),
            has_namespace_declarations: false,
            raw_source_registration,
        })
    }

    /// Returns the retained XML source.
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Returns the source token used by copied raw node and attribute accessors.
    pub fn raw_source(&self) -> XmlRawSource<'_> {
        XmlRawSource {
            input: &self.input,
            owner: self.raw_source_registration.owner().0,
        }
    }

    /// Returns the parsed XML language version.
    pub fn version(&self) -> crate::XmlVersion {
        if self.xml11 {
            crate::XmlVersion::Xml11
        } else {
            crate::XmlVersion::Xml10
        }
    }

    /// Returns the retained XML declaration.
    pub fn declaration(&self) -> Option<&XmlProcessingInstruction> {
        self.metadata.declaration.as_ref()
    }

    /// Returns the retained document type declaration.
    pub fn doctype(&self) -> Option<&XmlDoctype> {
        self.metadata.doctype.as_ref()
    }

    /// Returns top-level comments and processing instructions before the document element.
    pub fn misc_before_root(&self) -> &[XmlNode] {
        &self.metadata.misc_before_root
    }

    /// Returns top-level comments and processing instructions after the document element.
    pub fn misc_after_root(&self) -> &[XmlNode] {
        &self.metadata.misc_after_root
    }

    /// Returns the options used to parse this compact document.
    pub fn parse_options(&self) -> crate::ParserConfig {
        self.config
    }

    /// Returns the document element identifier.
    pub fn root(&self) -> XmlViewNodeId {
        self.root
    }

    /// Returns the compact nodes in document order.
    pub fn nodes(&self) -> &[RawXmlNode] {
        &self.nodes
    }

    /// Iterates all valid node identifiers in document order.
    pub fn node_ids(&self) -> impl DoubleEndedIterator<Item = XmlViewNodeId> + ExactSizeIterator {
        (0..self.nodes.len()).map(XmlViewNodeId)
    }

    /// Returns all compact attributes in document order.
    pub fn attributes(&self) -> &[RawXmlAttribute] {
        &self.attributes
    }

    /// Returns counts for the document-element tree.
    pub fn tree_stats(&self) -> XmlTreeStats {
        self.stats
    }

    /// Returns node.
    #[inline(always)]
    pub fn node(&self, id: XmlViewNodeId) -> Option<&RawXmlNode> {
        self.nodes.get(id.0)
    }

    /// Returns node name.
    #[inline(always)]
    pub fn node_name(&self, id: XmlViewNodeId) -> Option<&str> {
        let node = self.node(id)?;
        matches!(
            node.kind(),
            XmlNodeKind::Element | XmlNodeKind::ProcessingInstruction
        )
        .then(|| raw_range(&self.input, node.name_start, node.name_len))?
    }

    #[inline(always)]
    /// Returns node value.
    pub fn node_value(&self, id: XmlViewNodeId) -> Option<&str> {
        raw_node_value(&self.input, self.node(id)?)
    }

    /// Returns attribute name.
    #[inline(always)]
    pub fn attribute_name(&self, index: usize) -> Option<&str> {
        let attribute = self.attributes.get(index)?;
        raw_range(&self.input, attribute.name_start, attribute.name_len)
    }

    /// Returns attribute value.
    #[inline(always)]
    pub fn attribute_value(&self, index: usize) -> Option<&str> {
        let attribute = self.attributes.get(index)?;
        raw_range(&self.input, attribute.value_start, attribute.value_len)
    }

    /// Iterates an element's immediate children in document order.
    pub fn children(&self, id: XmlViewNodeId) -> impl Iterator<Item = XmlViewNodeId> + '_ {
        let parent = self.node(id);
        let end = parent.map_or(0, RawXmlNode::next_subtree);
        let first = parent.and_then(RawXmlNode::first_child);
        std::iter::successors(first, move |child| {
            let next = self.node(*child)?.next_subtree();
            (next < end).then_some(XmlViewNodeId(next))
        })
    }

    /// Returns the document element as a navigable node handle.
    pub fn root_node(&self) -> XmlCompactNode<'_> {
        XmlCompactNode {
            document: self,
            id: self.root,
        }
    }

    /// Returns a navigable node handle when `id` belongs to this document.
    pub fn node_ref(&self, id: XmlViewNodeId) -> Option<XmlCompactNode<'_>> {
        self.node(id).map(|_| XmlCompactNode { document: self, id })
    }

    pub(crate) fn materialize_element(&self, id: XmlViewNodeId) -> crate::XmlResult<XmlElement> {
        let record = self
            .node(id)
            .expect("compact child identifiers always reference a record");
        debug_assert_eq!(record.kind(), XmlNodeKind::Element);
        let mut attributes = Vec::with_capacity(record.attribute_count as usize);
        for index in record.attribute_range() {
            let raw = &self.attributes[index];
            attributes.push(XmlAttribute {
                name: raw_range(&self.input, raw.name_start, raw.name_len)
                    .expect("validated compact attribute name range")
                    .to_owned(),
                value: crate::parser::decode_compact_lexeme(
                    raw_range(&self.input, raw.value_start, raw.value_len)
                        .expect("validated compact attribute value range"),
                    crate::parser::CompactLexemeKind::Attribute,
                    self.xml11,
                    self.config.attribute_whitespace,
                )?,
            });
        }

        let mut children = Vec::new();
        for child in self.children(id) {
            let record = self.node(child).expect("compact child record exists");
            let primary = || {
                raw_range(&self.input, record.name_start, record.name_len)
                    .expect("validated compact node value range")
            };
            let node = match record.kind() {
                XmlNodeKind::Element => XmlNode::Element(self.materialize_element(child)?),
                XmlNodeKind::Text => XmlNode::Text(crate::parser::decode_compact_lexeme(
                    primary(),
                    crate::parser::CompactLexemeKind::Text,
                    self.xml11,
                    self.config.attribute_whitespace,
                )?),
                XmlNodeKind::Comment => XmlNode::Comment(crate::parser::decode_compact_lexeme(
                    primary(),
                    crate::parser::CompactLexemeKind::Opaque,
                    self.xml11,
                    self.config.attribute_whitespace,
                )?),
                XmlNodeKind::Cdata => XmlNode::Cdata(crate::parser::decode_compact_lexeme(
                    primary(),
                    crate::parser::CompactLexemeKind::Opaque,
                    self.xml11,
                    self.config.attribute_whitespace,
                )?),
                XmlNodeKind::ProcessingInstruction => {
                    XmlNode::ProcessingInstruction(XmlProcessingInstruction {
                        target: primary().to_owned(),
                        data: crate::parser::decode_compact_lexeme(
                            raw_range(&self.input, record.attribute_start, record.attribute_count)
                                .expect("validated processing-instruction data range"),
                            crate::parser::CompactLexemeKind::Opaque,
                            self.xml11,
                            self.config.attribute_whitespace,
                        )?,
                    })
                }
            };
            children.push(node);
        }

        Ok(XmlElement {
            name: raw_range(&self.input, record.name_start, record.name_len)
                .expect("validated compact element name range")
                .to_owned(),
            attributes,
            children,
        })
    }
}

impl<'a> XmlDocumentView<'a> {
    /// Returns the source text borrowed by this view.
    pub fn input(&self) -> &'a str {
        self.input
    }

    /// Returns the source token used by copied raw node and attribute accessors.
    pub fn raw_source(&self) -> XmlRawSource<'a> {
        XmlRawSource {
            input: self.input,
            owner: self.raw_source_registration.owner().0,
        }
    }

    /// Returns the parsed XML language version.
    pub fn version(&self) -> crate::XmlVersion {
        if self.xml11 {
            crate::XmlVersion::Xml11
        } else {
            crate::XmlVersion::Xml10
        }
    }

    /// Returns the document element's node identifier.
    pub fn root(&self) -> XmlViewNodeId {
        self.root
    }

    /// Returns all compact nodes in document order.
    pub fn nodes(&self) -> &[RawXmlNode] {
        &self.nodes
    }

    /// Iterates valid node identifiers in document order.
    pub fn node_ids(&self) -> impl DoubleEndedIterator<Item = XmlViewNodeId> + ExactSizeIterator {
        (0..self.nodes.len()).map(XmlViewNodeId)
    }

    /// Returns all compact attributes in document order.
    pub fn attributes(&self) -> &[RawXmlAttribute] {
        &self.attributes
    }

    /// Returns counts for the document-element tree.
    pub fn tree_stats(&self) -> XmlTreeStats {
        self.stats
    }

    /// Returns a compact node belonging to this view.
    pub fn node(&self, id: XmlViewNodeId) -> Option<&RawXmlNode> {
        self.nodes.get(id.0)
    }

    /// Returns a compact attribute by its document-order index.
    pub fn attribute(&self, index: usize) -> Option<&RawXmlAttribute> {
        self.attributes.get(index)
    }

    /// Returns an element name or processing-instruction target.
    pub fn node_name(&self, id: XmlViewNodeId) -> Option<&'a str> {
        raw_node_name(self.input, self.node(id)?)
    }

    /// Returns text, CDATA, comment, or processing-instruction data.
    #[inline(always)]
    pub fn node_value(&self, id: XmlViewNodeId) -> Option<&'a str> {
        raw_node_value(self.input, self.node(id)?)
    }

    /// Returns attribute name.
    pub fn attribute_name(&self, index: usize) -> Option<&'a str> {
        let attribute = self.attributes.get(index)?;
        raw_range(self.input, attribute.name_start, attribute.name_len)
    }

    /// Returns attribute value.
    pub fn attribute_value(&self, index: usize) -> Option<&'a str> {
        let attribute = self.attributes.get(index)?;
        raw_range(self.input, attribute.value_start, attribute.value_len)
    }

    /// Iterates an element's immediate children in document order.
    pub fn children(&self, id: XmlViewNodeId) -> impl Iterator<Item = XmlViewNodeId> + use<'_, 'a> {
        let parent = self.node(id);
        let end = parent.map_or(0, RawXmlNode::next_subtree);
        let first = parent.and_then(RawXmlNode::first_child);
        std::iter::successors(first, move |child| {
            let next = self.node(*child)?.next_subtree();
            (next < end).then_some(XmlViewNodeId(next))
        })
    }

    /// Returns the document element as a navigable node handle.
    pub fn root_node(&self) -> XmlViewNode<'_, 'a> {
        XmlViewNode {
            document: self,
            id: self.root,
        }
    }

    /// Returns a navigable node handle when `id` belongs to this view.
    pub fn node_ref(&self, id: XmlViewNodeId) -> Option<XmlViewNode<'_, 'a>> {
        self.node(id).map(|_| XmlViewNode { document: self, id })
    }
}

/// A borrowed attribute name and raw lexical value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct XmlAttributeView<'a> {
    name: &'a str,
    raw_value: &'a str,
}

impl<'a> XmlAttributeView<'a> {
    /// Returns the attribute name.
    pub const fn name(self) -> &'a str {
        self.name
    }

    /// Returns the unescaped lexical value from the retained source.
    pub const fn raw_value(self) -> &'a str {
        self.raw_value
    }
}

/// A navigable node in an [`XmlCompactDocument`].
#[derive(Clone, Copy, Debug)]
pub struct XmlCompactNode<'a> {
    document: &'a XmlCompactDocument,
    id: XmlViewNodeId,
}

impl<'a> XmlCompactNode<'a> {
    /// Returns this node's compact identifier.
    pub const fn id(self) -> XmlViewNodeId {
        self.id
    }

    /// Returns this node's semantic kind.
    pub fn kind(self) -> XmlNodeKind {
        self.document
            .node(self.id)
            .expect("compact node handle remains in its document")
            .kind()
    }

    /// Returns an element name or processing-instruction target.
    pub fn name(self) -> Option<&'a str> {
        self.document.node_name(self.id)
    }

    /// Returns retained text, CDATA, comment, or processing-instruction data.
    pub fn raw_value(self) -> Option<&'a str> {
        self.document.node_value(self.id)
    }

    /// Iterates element attributes in document order.
    pub fn attributes(self) -> impl DoubleEndedIterator<Item = XmlAttributeView<'a>> + 'a {
        let range = self
            .document
            .node(self.id)
            .filter(|node| node.kind() == XmlNodeKind::Element)
            .map_or(0..0, RawXmlNode::attribute_range);
        range.map(move |index| XmlAttributeView {
            name: self
                .document
                .attribute_name(index)
                .expect("validated compact attribute name"),
            raw_value: self
                .document
                .attribute_value(index)
                .expect("validated compact attribute value"),
        })
    }

    /// Returns an attribute by name.
    pub fn attribute(self, name: &str) -> Option<XmlAttributeView<'a>> {
        self.attributes().find(|attribute| attribute.name == name)
    }

    /// Iterates immediate children in document order.
    pub fn children(self) -> impl Iterator<Item = Self> + 'a {
        self.document.children(self.id).map(move |id| Self {
            document: self.document,
            id,
        })
    }

    /// Returns the first child element with `name`.
    pub fn child(self, name: &str) -> Option<Self> {
        self.children()
            .find(|child| child.kind() == XmlNodeKind::Element && child.name() == Some(name))
    }

    /// Iterates child elements with `name`.
    pub fn children_named<'name>(self, name: &'name str) -> impl Iterator<Item = Self> + 'name
    where
        'a: 'name,
    {
        self.children()
            .filter(move |child| child.kind() == XmlNodeKind::Element && child.name() == Some(name))
    }
}

/// A navigable node in a borrowed [`XmlDocumentView`].
#[derive(Clone, Copy, Debug)]
pub struct XmlViewNode<'document, 'input> {
    document: &'document XmlDocumentView<'input>,
    id: XmlViewNodeId,
}

impl<'document, 'input> XmlViewNode<'document, 'input> {
    /// Returns this node's compact identifier.
    pub const fn id(self) -> XmlViewNodeId {
        self.id
    }

    /// Returns this node's semantic kind.
    pub fn kind(self) -> XmlNodeKind {
        self.document
            .node(self.id)
            .expect("view node handle remains in its document")
            .kind()
    }

    /// Returns an element name or processing-instruction target.
    pub fn name(self) -> Option<&'input str> {
        self.document.node_name(self.id)
    }

    /// Returns retained text, CDATA, comment, or processing-instruction data.
    pub fn raw_value(self) -> Option<&'input str> {
        self.document.node_value(self.id)
    }

    /// Iterates element attributes in document order.
    pub fn attributes(
        self,
    ) -> impl DoubleEndedIterator<Item = XmlAttributeView<'input>> + 'document {
        let range = self
            .document
            .node(self.id)
            .filter(|node| node.kind() == XmlNodeKind::Element)
            .map_or(0..0, RawXmlNode::attribute_range);
        range.map(move |index| XmlAttributeView {
            name: self
                .document
                .attribute_name(index)
                .expect("validated view attribute name"),
            raw_value: self
                .document
                .attribute_value(index)
                .expect("validated view attribute value"),
        })
    }

    /// Returns an attribute by name.
    pub fn attribute(self, name: &str) -> Option<XmlAttributeView<'input>> {
        self.attributes().find(|attribute| attribute.name == name)
    }

    /// Iterates immediate children in document order.
    pub fn children(self) -> impl Iterator<Item = Self> + 'document {
        self.document.children(self.id).map(move |id| Self {
            document: self.document,
            id,
        })
    }

    /// Returns the first child element with `name`.
    pub fn child(self, name: &str) -> Option<Self> {
        self.children()
            .find(|child| child.kind() == XmlNodeKind::Element && child.name() == Some(name))
    }

    /// Iterates child elements with `name`.
    pub fn children_named<'name>(self, name: &'name str) -> impl Iterator<Item = Self> + 'name
    where
        'document: 'name,
    {
        self.children()
            .filter(move |child| child.kind() == XmlNodeKind::Element && child.name() == Some(name))
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
/// A public `XmlViewNodeId` value in the XML data model.
pub struct XmlViewNodeId(pub(crate) usize);

impl XmlViewNodeId {
    /// Returns this identifier's document-order index.
    pub fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// A public `RawXmlNode` value in the XML data model.
pub struct RawXmlNode {
    pub(crate) name_start: u32,
    pub(crate) name_len: u32,
    pub(crate) attribute_start: u32,
    pub(crate) attribute_count: u32,
    pub(crate) first_child: u32,
    next_subtree_and_kind: u32,
    owner: u32,
}

impl RawXmlNode {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_with_owner(
        owner: RawSourceOwner,
        kind: XmlNodeKind,
        name_start: u32,
        name_len: u32,
        attribute_start: u32,
        attribute_count: u32,
        first_child: u32,
        next_subtree: u32,
    ) -> Self {
        Self {
            name_start,
            name_len,
            attribute_start,
            attribute_count,
            first_child,
            next_subtree_and_kind: encode_next_subtree_and_kind(next_subtree, kind),
            owner: owner.0,
        }
    }

    /// Returns kind.
    #[inline(always)]
    pub fn kind(&self) -> XmlNodeKind {
        match self.next_subtree_and_kind >> 29 {
            0 => XmlNodeKind::Element,
            1 => XmlNodeKind::Text,
            2 => XmlNodeKind::Comment,
            3 => XmlNodeKind::Cdata,
            _ => XmlNodeKind::ProcessingInstruction,
        }
    }

    /// Returns next subtree.
    pub fn next_subtree(&self) -> usize {
        (self.next_subtree_and_kind & NODE_INDEX_MASK) as usize
    }

    /// Returns the immediate first child of an element.
    pub fn first_child(&self) -> Option<XmlViewNodeId> {
        (self.kind() == XmlNodeKind::Element && self.first_child != u32::MAX)
            .then_some(XmlViewNodeId(self.first_child as usize))
    }

    /// Returns the document-order range containing this element's attributes.
    #[inline(always)]
    pub fn attribute_range(&self) -> std::ops::Range<usize> {
        let start = self.attribute_start as usize;
        start..start + self.attribute_count as usize
    }

    pub(crate) fn set_element_next_subtree(&mut self, next_subtree: u32) {
        debug_assert_eq!(self.kind(), XmlNodeKind::Element);
        debug_assert!(next_subtree <= NODE_INDEX_MASK);
        self.next_subtree_and_kind = next_subtree;
    }

    /// Returns the name while `input` is registered to the originating parse.
    ///
    /// Prefer [`Self::name_with_source`] for a copied record. This compatibility accessor accepts
    /// the original string only while its document or view remains alive; an unrelated parse is
    /// rejected even if its allocation reuses the same address.
    pub fn name<'a>(&self, input: &'a str) -> Option<&'a str> {
        if !raw_source_is_registered(input, self.owner)
            || !matches!(
                self.kind(),
                XmlNodeKind::Element | XmlNodeKind::ProcessingInstruction
            )
        {
            return None;
        }
        raw_node_name(input, self)
    }

    /// Returns the value while `input` is registered to the originating parse.
    ///
    /// Prefer [`Self::value_with_source`] for a copied record. This compatibility accessor accepts
    /// the original string only while its document or view remains alive; an unrelated parse is
    /// rejected even if its allocation reuses the same address.
    pub fn value<'a>(&self, input: &'a str) -> Option<&'a str> {
        if !raw_source_is_registered(input, self.owner) {
            return None;
        }
        raw_node_value(input, self)
    }

    /// Returns the name when `source` belongs to the document that supplied this record.
    #[inline(always)]
    pub fn name_with_source<'a>(&self, source: XmlRawSource<'a>) -> Option<&'a str> {
        if self.owner != source.owner {
            return None;
        }
        raw_node_name(source.input, self)
    }

    /// Returns the value when `source` belongs to the document that supplied this record.
    #[inline(always)]
    pub fn value_with_source<'a>(&self, source: XmlRawSource<'a>) -> Option<&'a str> {
        if self.owner != source.owner {
            return None;
        }
        raw_node_value(source.input, self)
    }
}

#[inline(always)]
fn raw_node_name<'a>(input: &'a str, node: &RawXmlNode) -> Option<&'a str> {
    matches!(
        node.kind(),
        XmlNodeKind::Element | XmlNodeKind::ProcessingInstruction
    )
    .then(|| raw_range(input, node.name_start, node.name_len))?
}

#[inline(always)]
fn raw_node_value<'a>(input: &'a str, node: &RawXmlNode) -> Option<&'a str> {
    match node.kind() {
        XmlNodeKind::Text | XmlNodeKind::Comment | XmlNodeKind::Cdata => {
            raw_range(input, node.name_start, node.name_len)
        }
        XmlNodeKind::ProcessingInstruction => {
            raw_range(input, node.attribute_start, node.attribute_count)
        }
        XmlNodeKind::Element => None,
    }
}

#[inline(always)]
fn raw_range(input: &str, start: u32, len: u32) -> Option<&str> {
    let start = start as usize;
    let end = start.checked_add(len as usize)?;
    input.get(start..end)
}

const NODE_INDEX_MASK: u32 = (1 << 29) - 1;

fn encode_next_subtree_and_kind(next_subtree: u32, kind: XmlNodeKind) -> u32 {
    debug_assert!(next_subtree <= NODE_INDEX_MASK);
    ((kind as u32) << 29) | next_subtree
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// A public `RawXmlAttribute` value in the XML data model.
pub struct RawXmlAttribute {
    pub(crate) name_start: u32,
    pub(crate) name_len: u32,
    pub(crate) value_start: u32,
    pub(crate) value_len: u32,
    owner: u32,
}

impl RawXmlAttribute {
    pub(crate) fn new(
        owner: RawSourceOwner,
        name_start: u32,
        name_len: u32,
        value_start: u32,
        value_len: u32,
    ) -> Self {
        Self {
            name_start,
            name_len,
            value_start,
            value_len,
            owner: owner.0,
        }
    }

    /// Returns the name when `source` belongs to the document that supplied this attribute.
    ///
    /// The opaque token rejects unrelated parses even if their input allocation reuses the
    /// original source address. Prefer the document-bound attribute accessors on
    /// [`XmlDocumentView`] and [`XmlCompactDocument`] when the containing document is available.
    pub fn name<'a>(&self, source: XmlRawSource<'a>) -> Option<&'a str> {
        if self.owner != source.owner {
            return None;
        }
        raw_range(source.input, self.name_start, self.name_len)
    }

    /// Returns the raw lexical value when `input` supplied this attribute.
    ///
    /// Entity and character references remain encoded. Use the document-bound APIs when possible;
    /// they avoid asking the caller to provide the source explicitly.
    pub fn value<'a>(&self, source: XmlRawSource<'a>) -> Option<&'a str> {
        if self.owner != source.owner {
            return None;
        }
        raw_range(source.input, self.value_start, self.value_len)
    }
}
