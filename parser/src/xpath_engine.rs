use std::{
    borrow::Cow,
    collections::{BTreeMap, HashSet},
    error::Error,
    fmt,
    str::FromStr,
};

use crate::{
    XmlAttribute, XmlElement, XmlNamespace, XmlNode, XmlProcessingInstruction, XmlQualifiedName,
    XML_NAMESPACE_URI,
};

const MAX_XPATH_EXPRESSION_DEPTH: usize = 96;
const XPATH_EXPRESSION_DEPTH_ERROR: &str = "XPath expression depth limit exceeded";

/// A node selected from a standalone element tree.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum XPathNode<'a> {
    /// Indicates `Element`.
    Element(&'a XmlElement),
    /// Indicates `Attribute`.
    Attribute(&'a XmlAttribute),
    /// Indicates `Text`.
    Text(&'a str),
    /// Indicates `Comment`.
    Comment(&'a str),
    /// Indicates `ProcessingInstruction`.
    ProcessingInstruction(&'a XmlProcessingInstruction),
    /// Indicates `Namespace`.
    Namespace {
        /// The owner.
        owner: &'a XmlElement,
        /// The namespace.
        namespace: XmlNamespace<'a>,
    },
}

impl<'a> XPathNode<'a> {
    /// Returns this value as element when it has that kind.
    pub fn as_element(self) -> Option<&'a XmlElement> {
        match self {
            Self::Element(element) => Some(element),
            _ => None,
        }
    }

    /// Returns this value as attribute when it has that kind.
    pub fn as_attribute(self) -> Option<&'a XmlAttribute> {
        match self {
            Self::Attribute(attribute) => Some(attribute),
            _ => None,
        }
    }

    /// Returns string value.
    pub fn string_value(self) -> String {
        match self {
            Self::Element(element) => element.text_content(),
            Self::Attribute(attribute) => attribute.value.clone(),
            Self::Text(value) | Self::Comment(value) => value.to_owned(),
            Self::ProcessingInstruction(pi) => pi.data.clone(),
            Self::Namespace { namespace, .. } => namespace.uri.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// A public `XPathError` value in the XML data model.
pub struct XPathError {
    /// The message.
    pub message: &'static str,
    /// The byte.
    pub byte: usize,
}

impl fmt::Display for XPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at byte {}", self.message, self.byte)
    }
}

impl Error for XPathError {}

/// A reusable parsed XPath expression.
#[derive(Clone, Debug)]
pub struct XPathExpression {
    compiled: CompiledXPath,
}

impl XPathExpression {
    pub(crate) fn simple_descendant_filter(&self) -> Option<SimpleDescendantFilter> {
        let CompiledXPath::Nodes(query) = &self.compiled else {
            return None;
        };
        query.simple_descendant_filter()
    }
}

/// A scalar value supplied to an XPath variable.
#[derive(Clone, Debug, PartialEq)]
pub enum XPathVariable {
    /// Indicates `Boolean`.
    Boolean(bool),
    /// Indicates `Number`.
    Number(f64),
    /// Indicates `String`.
    String(String),
}

impl From<bool> for XPathVariable {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}

impl From<f64> for XPathVariable {
    fn from(value: f64) -> Self {
        Self::Number(value)
    }
}

impl From<String> for XPathVariable {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for XPathVariable {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

/// Scalar bindings used by a compiled XPath expression.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct XPathVariables {
    values: BTreeMap<String, XPathVariable>,
}

/// Prefix-to-URI bindings used by namespace-aware XPath name tests.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct XPathNamespaces {
    values: BTreeMap<String, String>,
}

impl XPathNamespaces {
    /// Binds a non-empty XPath prefix to a non-empty namespace URI.
    ///
    /// Returns the previous URI when the prefix was already bound. XPath does not use an XML
    /// document's default namespace for unprefixed name tests, so callers must bind and use an
    /// explicit prefix when selecting elements in a default namespace.
    pub fn bind(
        &mut self,
        prefix: impl Into<String>,
        namespace_uri: impl Into<String>,
    ) -> Result<Option<String>, XPathError> {
        let prefix = prefix.into();
        if prefix.is_empty()
            || !crate::syntax::is_name_start_char(prefix.chars().next().unwrap())
            || !prefix.chars().all(crate::syntax::is_name_char)
        {
            return Err(XPathError {
                message: "invalid XPath namespace prefix",
                byte: 0,
            });
        }
        let namespace_uri = namespace_uri.into();
        if namespace_uri.is_empty() {
            return Err(XPathError {
                message: "empty XPath namespace URI",
                byte: 0,
            });
        }
        Ok(self.values.insert(prefix, namespace_uri))
    }

    /// Returns the URI bound to a prefix.
    ///
    /// The reserved `xml` prefix is always available even when it was not explicitly bound.
    pub fn get(&self, prefix: &str) -> Option<&str> {
        if prefix == "xml" {
            Some(XML_NAMESPACE_URI)
        } else {
            self.values.get(prefix).map(String::as_str)
        }
    }
}

/// Variables and namespace bindings for one XPath evaluation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct XPathContext {
    /// Scalar variable bindings used by `$name` expressions.
    pub variables: XPathVariables,
    /// Prefix bindings used by namespace-aware name tests.
    pub namespaces: XPathNamespaces,
}

/// A compact, string-sharing tree used to evaluate XPath for [`crate::XmlDom`].
///
/// This index borrows unchanged names and character data. It is built only for an XPath call and
/// stores parent/sibling links in flat vectors.
#[derive(Debug)]
pub(crate) struct XPathArena<'a> {
    nodes: Vec<XPathArenaNode<'a>>,
    attributes: Vec<XPathArenaAttribute<'a>>,
    processing_instruction_values: Vec<Cow<'a, str>>,
    root: usize,
    has_namespaces: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum XPathArenaNodeKind {
    Element,
    Text,
    Comment,
    ProcessingInstruction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum XPathArenaSelection {
    Element(usize),
    Attribute {
        owner: usize,
        name: String,
    },
    Text(usize),
    Comment(usize),
    ProcessingInstruction(usize),
    Namespace {
        owner: usize,
        prefix: Option<String>,
        uri: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum XPathArenaValue {
    Nodes(Vec<XPathArenaSelection>),
    Boolean(bool),
    Number(f64),
    String(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SimpleDescendantFilter {
    pub(crate) element_name: Option<String>,
    pub(crate) required_attributes: Vec<String>,
}

pub(crate) fn simple_descendant_filter(
    query: &str,
) -> Result<Option<SimpleDescendantFilter>, XPathError> {
    Query::parse(query).map(|query| query.simple_descendant_filter())
}

#[derive(Debug)]
struct XPathArenaNode<'a> {
    kind: XPathArenaNodeKind,
    primary: Cow<'a, str>,
    parent: u32,
    first_child: u32,
    last_child: u32,
    next_sibling: u32,
    child_index: u32,
    attribute_start: u32,
    attribute_count: u32,
    secondary: u32,
    subtree_end: u32,
}

#[derive(Debug)]
struct XPathArenaAttribute<'a> {
    owner: usize,
    name: Cow<'a, str>,
    value: Cow<'a, str>,
}

impl<'a> XPathArena<'a> {
    pub(crate) fn with_capacity(nodes: usize, attributes: usize) -> Self {
        Self {
            nodes: Vec::with_capacity(nodes),
            attributes: Vec::with_capacity(attributes),
            processing_instruction_values: Vec::new(),
            root: 0,
            has_namespaces: false,
        }
    }

    pub(crate) fn push_node(
        &mut self,
        kind: XPathArenaNodeKind,
        name: Cow<'a, str>,
        value: Cow<'a, str>,
        parent: Option<usize>,
        child_index: usize,
    ) -> usize {
        let index = self.nodes.len();
        let (primary, secondary) = if kind == XPathArenaNodeKind::ProcessingInstruction {
            let secondary = self.processing_instruction_values.len();
            self.processing_instruction_values.push(value);
            (name, compact_xpath_index(secondary))
        } else if kind == XPathArenaNodeKind::Element {
            (name, u32::MAX)
        } else {
            (value, u32::MAX)
        };
        self.nodes.push(XPathArenaNode {
            kind,
            primary,
            parent: parent.map_or(u32::MAX, compact_xpath_index),
            first_child: u32::MAX,
            last_child: u32::MAX,
            next_sibling: u32::MAX,
            child_index: compact_xpath_index(child_index),
            attribute_start: compact_xpath_index(self.attributes.len()),
            attribute_count: 0,
            secondary,
            subtree_end: if kind == XPathArenaNodeKind::Element {
                u32::MAX
            } else {
                compact_xpath_index(index + 1)
            },
        });
        if let Some(parent) = parent {
            if let Some(previous) = expand_xpath_index(self.nodes[parent].last_child) {
                self.nodes[previous].next_sibling = compact_xpath_index(index);
            } else {
                self.nodes[parent].first_child = compact_xpath_index(index);
            }
            self.nodes[parent].last_child = compact_xpath_index(index);
        } else {
            self.root = index;
        }
        index
    }

    pub(crate) fn push_attribute(&mut self, owner: usize, name: Cow<'a, str>, value: Cow<'a, str>) {
        debug_assert_eq!(
            self.nodes[owner].attribute_start + self.nodes[owner].attribute_count,
            compact_xpath_index(self.attributes.len())
        );
        self.has_namespaces |= is_namespace_declaration(&name);
        self.attributes
            .push(XPathArenaAttribute { owner, name, value });
        self.nodes[owner].attribute_count = self.nodes[owner]
            .attribute_count
            .checked_add(1)
            .expect("XPath attribute count fits u32");
    }

    pub(crate) fn close_element(&mut self, element: usize) {
        debug_assert_eq!(self.nodes[element].kind, XPathArenaNodeKind::Element);
        self.nodes[element].subtree_end = compact_xpath_index(self.nodes.len());
    }

    pub(crate) fn element_path(&self, node: usize) -> Option<Vec<usize>> {
        (self.nodes.get(node)?.kind == XPathArenaNodeKind::Element).then_some(())?;
        self.node_path(node)
    }

    pub(crate) fn node_path(&self, mut node: usize) -> Option<Vec<usize>> {
        self.nodes.get(node)?;
        let mut reversed = Vec::new();
        while let Some(parent) = expand_xpath_index(self.nodes[node].parent) {
            reversed.push(self.nodes[node].child_index as usize);
            node = parent;
        }
        reversed.reverse();
        Some(reversed)
    }

    pub(crate) fn element_at_path(&self, path: &[usize]) -> Option<usize> {
        let mut node = self.root;
        for &requested in path {
            let mut child = expand_xpath_index(self.nodes[node].first_child);
            let mut index = 0usize;
            loop {
                let candidate = child?;
                if index == requested {
                    node = candidate;
                    break;
                }
                child = expand_xpath_index(self.nodes[candidate].next_sibling);
                index += 1;
            }
        }
        (self.nodes[node].kind == XPathArenaNodeKind::Element).then_some(node)
    }

    pub(crate) fn select_elements(
        &'a self,
        query: &str,
        context: Option<usize>,
    ) -> Result<Vec<XPathArenaSelection>, XPathError> {
        let query = Query::parse(query)?;
        if context.is_some() && query.paths.iter().any(|path| path.absolute) {
            return Err(XPathError {
                message: "absolute XPath requires XmlDom context",
                byte: 0,
            });
        }
        let tree = Tree::arena(self);
        let initial = context.map_or(EvalNode::Document, EvalNode::ArenaElement);
        Ok(query
            .evaluate_raw(
                tree,
                initial,
                &XPathVariables::default(),
                &XPathNamespaces::default(),
            )?
            .into_iter()
            .map(|node| arena_selection(self, node))
            .collect())
    }

    pub(crate) fn select_string(
        &'a self,
        query: &str,
        context: usize,
    ) -> Result<Option<String>, XPathError> {
        let query = Query::parse(query)?;
        if query.paths.iter().any(|path| path.absolute) {
            return Err(XPathError {
                message: "absolute XPath requires XmlDom context",
                byte: 0,
            });
        }
        let tree = Tree::arena(self);
        let nodes = query.evaluate_raw(
            tree,
            EvalNode::ArenaElement(context),
            &XPathVariables::default(),
            &XPathNamespaces::default(),
        )?;
        Ok(nodes.first().map(|node| node_string(tree, *node)))
    }

    pub(crate) fn evaluate(
        &'a self,
        expression: &XPathExpression,
        context: Option<usize>,
        bindings: &XPathContext,
    ) -> Result<XPathArenaValue, XPathError> {
        let tree = Tree::arena(self);
        let initial = context.map_or(EvalNode::Document, EvalNode::ArenaElement);
        Ok(
            match expression.evaluate_internal(
                tree,
                initial,
                &bindings.variables,
                &bindings.namespaces,
            )? {
                ScalarValue::Nodes(nodes) => XPathArenaValue::Nodes(
                    tree.order(nodes)
                        .into_iter()
                        .map(|node| arena_selection(self, node))
                        .collect(),
                ),
                ScalarValue::Bool(value) => XPathArenaValue::Boolean(value),
                ScalarValue::Number(value) => XPathArenaValue::Number(value),
                ScalarValue::String(value) => XPathArenaValue::String(value),
            },
        )
    }

    pub(crate) fn evaluate_boolean(
        &'a self,
        expression: &XPathExpression,
        context: Option<usize>,
        bindings: &XPathContext,
    ) -> Result<bool, XPathError> {
        let tree = Tree::arena(self);
        Ok(expression
            .evaluate_internal(
                tree,
                context.map_or(EvalNode::Document, EvalNode::ArenaElement),
                &bindings.variables,
                &bindings.namespaces,
            )?
            .into_bool())
    }

    pub(crate) fn evaluate_number(
        &'a self,
        expression: &XPathExpression,
        context: Option<usize>,
        bindings: &XPathContext,
    ) -> Result<f64, XPathError> {
        let tree = Tree::arena(self);
        Ok(expression
            .evaluate_internal(
                tree,
                context.map_or(EvalNode::Document, EvalNode::ArenaElement),
                &bindings.variables,
                &bindings.namespaces,
            )?
            .into_number(tree))
    }

    pub(crate) fn evaluate_string(
        &'a self,
        expression: &XPathExpression,
        context: Option<usize>,
        bindings: &XPathContext,
    ) -> Result<String, XPathError> {
        let tree = Tree::arena(self);
        Ok(expression
            .evaluate_internal(
                tree,
                context.map_or(EvalNode::Document, EvalNode::ArenaElement),
                &bindings.variables,
                &bindings.namespaces,
            )?
            .into_string(tree))
    }
}

fn compact_xpath_index(value: usize) -> u32 {
    u32::try_from(value).expect("XPath arena index exceeds u32")
}

fn expand_xpath_index(value: u32) -> Option<usize> {
    (value != u32::MAX).then_some(value as usize)
}

fn arena_selection(arena: &XPathArena<'_>, node: EvalNode<'_>) -> XPathArenaSelection {
    match node {
        EvalNode::ArenaElement(index) => XPathArenaSelection::Element(index),
        EvalNode::ArenaAttribute(index) => XPathArenaSelection::Attribute {
            owner: arena.attributes[index].owner,
            name: arena.attributes[index].name.to_string(),
        },
        EvalNode::ArenaText(index) => XPathArenaSelection::Text(index),
        EvalNode::ArenaComment(index) => XPathArenaSelection::Comment(index),
        EvalNode::ArenaProcessingInstruction(index) => {
            XPathArenaSelection::ProcessingInstruction(index)
        }
        EvalNode::ArenaNamespace { owner, prefix, uri } => XPathArenaSelection::Namespace {
            owner,
            prefix: prefix.map(str::to_owned),
            uri: uri.to_owned(),
        },
        _ => unreachable!("XPathArena evaluation only yields arena-backed nodes"),
    }
}

impl XPathVariables {
    pub(crate) fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Inserts a variable name without the leading `$`.
    pub fn insert(
        &mut self,
        name: impl Into<String>,
        value: impl Into<XPathVariable>,
    ) -> Result<Option<XPathVariable>, XPathError> {
        let name = name.into();
        crate::XmlQualifiedName::parse(&name).map_err(|_| XPathError {
            message: "invalid XPath variable name",
            byte: 0,
        })?;
        Ok(self.values.insert(name, value.into()))
    }

    /// Returns get.
    pub fn get(&self, name: &str) -> Option<&XPathVariable> {
        self.values.get(name)
    }
}

impl XPathExpression {
    /// Parses an XPath expression once for repeated evaluation through [`crate::XmlDom`].
    pub fn compile(source: &str) -> Result<Self, XPathError> {
        let compiled = match Query::parse(source) {
            Ok(query) => CompiledXPath::Nodes(query),
            Err(path_error) if path_error.message == XPATH_EXPRESSION_DEPTH_ERROR => {
                return Err(path_error);
            }
            Err(path_error) => match PredicateParser::new(source).parse() {
                Ok(expression) => CompiledXPath::Scalar(expression),
                Err(predicate_error) if predicate_error.message == XPATH_EXPRESSION_DEPTH_ERROR => {
                    return Err(predicate_error);
                }
                Err(_) => return Err(path_error),
            },
        };
        Ok(Self { compiled })
    }

    fn evaluate_internal<'a>(
        &self,
        tree: Tree<'a>,
        context: EvalNode<'a>,
        variables: &XPathVariables,
        namespaces: &XPathNamespaces,
    ) -> Result<ScalarValue<'a>, XPathError> {
        match &self.compiled {
            CompiledXPath::Nodes(query) => Ok(ScalarValue::Nodes(
                query.evaluate_raw(tree, context, variables, namespaces)?,
            )),
            CompiledXPath::Scalar(expression) => {
                evaluate_scalar(expression, tree, context, 1, 1, (variables, namespaces))
            }
        }
    }
}

impl FromStr for XPathExpression {
    type Err = XPathError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        Self::compile(source)
    }
}

impl XmlElement {
    /// Selects nodes.
    pub fn select_nodes(&self, query: &str) -> Result<Vec<XPathNode<'_>>, XPathError> {
        let query = Query::parse(query)?;
        if query.paths.iter().any(|path| path.absolute) {
            return Err(XPathError {
                message: "absolute XPath requires XmlDom context",
                byte: 0,
            });
        }
        query.evaluate(
            Tree::element(self),
            EvalNode::Element(self),
            &XPathVariables::default(),
            &XPathNamespaces::default(),
        )
    }

    /// Selects node.
    pub fn select_node(&self, query: &str) -> Result<Option<XPathNode<'_>>, XPathError> {
        Ok(self.select_nodes(query)?.into_iter().next())
    }

    /// Selects elements.
    pub fn select_elements(&self, query: &str) -> Result<Vec<&XmlElement>, XPathError> {
        Ok(self
            .select_nodes(query)?
            .into_iter()
            .filter_map(XPathNode::as_element)
            .collect())
    }

    /// Selects string.
    pub fn select_string(&self, query: &str) -> Result<Option<String>, XPathError> {
        Ok(self.select_node(query)?.map(XPathNode::string_value))
    }
}

#[derive(Clone, Copy)]
enum Tree<'a> {
    Element(&'a XmlElement),
    Arena(&'a XPathArena<'a>),
}

impl<'a> Tree<'a> {
    fn element(root: &'a XmlElement) -> Self {
        Self::Element(root)
    }

    fn arena(arena: &'a XPathArena<'a>) -> Self {
        Self::Arena(arena)
    }

    fn root(self) -> EvalNode<'a> {
        match self {
            Self::Element(root) => EvalNode::Element(root),
            Self::Arena(arena) => EvalNode::ArenaElement(arena.root),
        }
    }

    fn has_document(self) -> bool {
        match self {
            Self::Element(_) => false,
            Self::Arena(_) => true,
        }
    }

    fn children(self, node: EvalNode<'a>) -> Vec<EvalNode<'a>> {
        match (self, node) {
            (_, EvalNode::Document) => vec![self.root()],
            (_, EvalNode::Element(element)) => {
                element.children.iter().map(EvalNode::from).collect()
            }
            (Self::Arena(arena), EvalNode::ArenaElement(index)) => {
                let mut output = Vec::new();
                let mut child = expand_xpath_index(arena.nodes[index].first_child);
                while let Some(index) = child {
                    output.push(arena_eval_node(arena, index));
                    child = expand_xpath_index(arena.nodes[index].next_sibling);
                }
                output
            }
            _ => Vec::new(),
        }
    }

    fn attributes(self, node: EvalNode<'a>) -> Vec<EvalNode<'a>> {
        match (self, node) {
            (_, EvalNode::Element(element)) => element
                .attributes
                .iter()
                .filter(|attribute| !attribute.is_namespace_declaration())
                .map(EvalNode::Attribute)
                .collect(),
            (Self::Arena(arena), EvalNode::ArenaElement(index)) => {
                let record = &arena.nodes[index];
                (record.attribute_start as usize
                    ..(record.attribute_start + record.attribute_count) as usize)
                    .filter(|attribute| {
                        !is_namespace_declaration(&arena.attributes[*attribute].name)
                    })
                    .map(EvalNode::ArenaAttribute)
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn descendants(self, node: EvalNode<'a>) -> Vec<EvalNode<'a>> {
        if let Self::Arena(arena) = self {
            let range = match node {
                EvalNode::Document => 0..arena.nodes.len(),
                EvalNode::ArenaElement(index) => index + 1..arena.nodes[index].subtree_end as usize,
                _ => return Vec::new(),
            };
            return range.map(|index| arena_eval_node(arena, index)).collect();
        }
        let mut output = Vec::new();
        let mut stack = self.children(node);
        stack.reverse();
        while let Some(candidate) = stack.pop() {
            let mut children = self.children(candidate);
            children.reverse();
            stack.extend(children);
            output.push(candidate);
        }
        output
    }

    fn parent(self, target: EvalNode<'a>) -> Option<EvalNode<'a>> {
        if same_eval_node(target, self.root()) {
            return self.has_document().then_some(EvalNode::Document);
        }
        if let Self::Arena(arena) = self {
            return match target {
                EvalNode::ArenaElement(index)
                | EvalNode::ArenaText(index)
                | EvalNode::ArenaComment(index)
                | EvalNode::ArenaProcessingInstruction(index) => {
                    expand_xpath_index(arena.nodes[index].parent).map(EvalNode::ArenaElement)
                }
                EvalNode::ArenaAttribute(index) => {
                    Some(EvalNode::ArenaElement(arena.attributes[index].owner))
                }
                EvalNode::ArenaNamespace { owner, .. } => Some(EvalNode::ArenaElement(owner)),
                _ => None,
            };
        }
        let Self::Element(root) = self else {
            unreachable!()
        };
        let mut elements = vec![root];
        while let Some(element) = elements.pop() {
            if element
                .attributes
                .iter()
                .any(|attribute| same_eval_node(target, EvalNode::Attribute(attribute)))
            {
                return Some(EvalNode::Element(element));
            }
            for child in &element.children {
                let candidate = EvalNode::from(child);
                if same_eval_node(target, candidate) {
                    return Some(EvalNode::Element(element));
                }
                if let XmlNode::Element(child) = child {
                    elements.push(child);
                }
            }
        }
        None
    }

    fn ancestors(self, node: EvalNode<'a>) -> Vec<EvalNode<'a>> {
        let mut output = Vec::new();
        let mut current = node;
        while let Some(parent) = self.parent(current) {
            output.push(parent);
            current = parent;
        }
        output
    }

    fn siblings(self, node: EvalNode<'a>, following: bool) -> Vec<EvalNode<'a>> {
        let Some(parent) = self.parent(node) else {
            return Vec::new();
        };
        let children = self.children(parent);
        let Some(index) = children
            .iter()
            .position(|candidate| same_eval_node(*candidate, node))
        else {
            return Vec::new();
        };
        if following {
            children.into_iter().skip(index + 1).collect()
        } else {
            children[..index].iter().copied().rev().collect()
        }
    }

    fn following(self, node: EvalNode<'a>) -> Vec<EvalNode<'a>> {
        if let Some(nodes) = self.arena_following_union(std::slice::from_ref(&node)) {
            return nodes;
        }
        let order = self.document_order();
        let Some(index) = order
            .iter()
            .position(|candidate| same_eval_node(*candidate, node))
        else {
            return Vec::new();
        };
        let descendants = self.descendants(node);
        order
            .into_iter()
            .skip(index + 1)
            .filter(|candidate| !is_attribute_node(*candidate))
            .filter(|candidate| {
                !descendants
                    .iter()
                    .any(|descendant| same_eval_node(*candidate, *descendant))
            })
            .collect()
    }

    #[inline(never)]
    fn arena_following_union(self, contexts: &[EvalNode<'a>]) -> Option<Vec<EvalNode<'a>>> {
        let Self::Arena(arena) = self else {
            return None;
        };
        let mut start = arena.nodes.len();
        for context in contexts {
            let candidate = match *context {
                EvalNode::Document => arena.nodes.len(),
                EvalNode::ArenaElement(index) => arena.nodes[index].subtree_end as usize,
                EvalNode::ArenaText(index)
                | EvalNode::ArenaComment(index)
                | EvalNode::ArenaProcessingInstruction(index) => index + 1,
                EvalNode::ArenaAttribute(index) => arena.attributes[index].owner + 1,
                EvalNode::ArenaNamespace { owner, .. } => owner + 1,
                _ => return None,
            };
            start = start.min(candidate);
        }
        Some(
            (start..arena.nodes.len())
                .map(|index| arena_eval_node(arena, index))
                .collect(),
        )
    }

    fn preceding(self, node: EvalNode<'a>) -> Vec<EvalNode<'a>> {
        if let Some(nodes) = self.arena_preceding_union(std::slice::from_ref(&node), true) {
            return nodes;
        }
        let order = self.document_order();
        let Some(index) = order
            .iter()
            .position(|candidate| same_eval_node(*candidate, node))
        else {
            return Vec::new();
        };
        let ancestors = self.ancestors(node);
        order[..index]
            .iter()
            .copied()
            .rev()
            .filter(|candidate| !is_attribute_node(*candidate))
            .filter(|candidate| {
                !ancestors
                    .iter()
                    .any(|ancestor| same_eval_node(*candidate, *ancestor))
            })
            .collect()
    }

    #[inline(never)]
    fn arena_preceding_union(
        self,
        contexts: &[EvalNode<'a>],
        reverse_axis_order: bool,
    ) -> Option<Vec<EvalNode<'a>>> {
        let Self::Arena(arena) = self else {
            return None;
        };
        // Preceding sets grow monotonically in document order, so their predicate-free union is
        // exactly the preceding set of the latest context. Predicate-bearing steps do not call
        // this multi-context path because their reverse-axis positions are context-relative.
        let mut latest = None;
        for context in contexts {
            let candidate = match *context {
                EvalNode::Document => continue,
                EvalNode::ArenaElement(index)
                | EvalNode::ArenaText(index)
                | EvalNode::ArenaComment(index)
                | EvalNode::ArenaProcessingInstruction(index) => {
                    (index, expand_xpath_index(arena.nodes[index].parent))
                }
                EvalNode::ArenaAttribute(index) => {
                    let owner = arena.attributes[index].owner;
                    (owner, Some(owner))
                }
                EvalNode::ArenaNamespace { owner, .. } => (owner, Some(owner)),
                _ => return None,
            };
            if latest.is_none_or(|current: (usize, Option<usize>)| candidate.0 > current.0) {
                latest = Some(candidate);
            }
        }
        let Some((end, mut ancestor)) = latest else {
            return Some(Vec::new());
        };

        let mut excluded = Vec::new();
        while let Some(index) = ancestor {
            if index < end {
                excluded.push(index);
            }
            ancestor = expand_xpath_index(arena.nodes[index].parent);
        }
        let mut output = Vec::with_capacity(end.saturating_sub(excluded.len()));
        if reverse_axis_order {
            let mut excluded = excluded.into_iter().peekable();
            for index in (0..end).rev() {
                if excluded.peek().is_some_and(|excluded| *excluded == index) {
                    excluded.next();
                } else {
                    output.push(arena_eval_node(arena, index));
                }
            }
        } else {
            excluded.reverse();
            let mut excluded = excluded.into_iter().peekable();
            for index in 0..end {
                if excluded.peek().is_some_and(|excluded| *excluded == index) {
                    excluded.next();
                } else {
                    output.push(arena_eval_node(arena, index));
                }
            }
        }
        Some(output)
    }

    fn document_order(self) -> Vec<EvalNode<'a>> {
        let mut output = Vec::new();
        if self.has_document() {
            output.push(EvalNode::Document);
        }
        match self {
            Self::Element(root) => append_element_order(root, &mut output),
            Self::Arena(arena) => append_arena_order(arena, arena.root, &mut output),
        }
        output
    }

    fn order(self, nodes: Vec<EvalNode<'a>>) -> Vec<EvalNode<'a>> {
        if let Self::Arena(arena) = self {
            let mut nodes = nodes;
            if !nodes
                .windows(2)
                .all(|pair| arena_order_key(arena, pair[0]) <= arena_order_key(arena, pair[1]))
            {
                nodes.sort_by(|left, right| {
                    arena_order_key(arena, *left).cmp(&arena_order_key(arena, *right))
                });
            }
            nodes.dedup_by(|left, right| same_eval_node(*left, *right));
            return nodes;
        }
        let mut selected = HashSet::with_capacity(nodes.len());
        let mut namespace_nodes_by_owner: BTreeMap<usize, Vec<EvalNode<'a>>> = BTreeMap::new();
        for node in nodes {
            if is_namespace_node(node) {
                namespace_nodes_by_owner
                    .entry(element_eval_key(
                        namespace_owner(node).expect("namespace owner"),
                    ))
                    .or_default()
                    .push(node);
            } else {
                selected.insert(node.key());
            }
        }

        let mut output = Vec::new();
        for candidate in self.document_order() {
            if selected.remove(&candidate.key()) {
                output.push(candidate);
            }
            if is_element_node(candidate) {
                let Some(mut namespace_nodes) =
                    namespace_nodes_by_owner.remove(&element_eval_key(candidate))
                else {
                    continue;
                };
                namespace_nodes.sort_by(|left, right| match (left, right) {
                    (
                        EvalNode::Namespace {
                            namespace: left, ..
                        },
                        EvalNode::Namespace {
                            namespace: right, ..
                        },
                    ) => left.prefix.cmp(&right.prefix),
                    (
                        EvalNode::ArenaNamespace { prefix: left, .. },
                        EvalNode::ArenaNamespace { prefix: right, .. },
                    ) => left.cmp(right),
                    _ => std::cmp::Ordering::Equal,
                });
                namespace_nodes.dedup_by(|left, right| same_eval_node(*left, *right));
                output.extend(namespace_nodes);
            }
        }
        output
    }

    fn path_to(self, target: &'a XmlElement) -> Option<Vec<&'a XmlElement>> {
        let Self::Element(root) = self else {
            return None;
        };
        let mut path = Vec::new();
        let mut stack = vec![(root, 0usize)];
        while let Some((element, child_index)) = stack.last_mut() {
            if *child_index == 0 {
                path.push(*element);
                if std::ptr::eq(*element, target) {
                    return Some(path);
                }
            }
            let next = element.children[*child_index..]
                .iter()
                .enumerate()
                .find_map(|(offset, node)| match node {
                    XmlNode::Element(child) => Some((offset, child)),
                    _ => None,
                });
            if let Some((offset, child)) = next {
                *child_index += offset + 1;
                stack.push((child, 0));
            } else {
                stack.pop();
                path.pop();
            }
        }
        None
    }

    fn namespaces(self, element: EvalNode<'a>) -> Vec<EvalNode<'a>> {
        let mut namespaces = vec![XmlNamespace {
            prefix: Some("xml"),
            uri: XML_NAMESPACE_URI,
        }];
        match (self, element) {
            (Self::Element(_), EvalNode::Element(element)) => {
                let Some(path) = self.path_to(element) else {
                    return Vec::new();
                };
                for ancestor in path.into_iter().rev() {
                    for namespace in ancestor.namespace_declarations() {
                        if !namespaces
                            .iter()
                            .any(|existing| existing.prefix == namespace.prefix)
                        {
                            namespaces.push(namespace);
                        }
                    }
                }
            }
            (Self::Arena(arena), EvalNode::ArenaElement(mut index)) => loop {
                let record = &arena.nodes[index];
                for attribute in &arena.attributes[record.attribute_start as usize
                    ..(record.attribute_start + record.attribute_count) as usize]
                {
                    let prefix = namespace_declaration_prefix(&attribute.name);
                    if let Some(prefix) = prefix {
                        let prefix = if prefix.is_empty() {
                            None
                        } else {
                            Some(prefix)
                        };
                        if !namespaces.iter().any(|existing| existing.prefix == prefix) {
                            namespaces.push(XmlNamespace {
                                prefix,
                                uri: &attribute.value,
                            });
                        }
                    }
                }
                let Some(parent) = expand_xpath_index(record.parent) else {
                    break;
                };
                index = parent;
            },
            _ => return Vec::new(),
        }
        namespaces
            .into_iter()
            .map(|namespace| match element {
                EvalNode::Element(owner) => EvalNode::Namespace { owner, namespace },
                EvalNode::ArenaElement(owner) => EvalNode::ArenaNamespace {
                    owner,
                    prefix: namespace.prefix,
                    uri: namespace.uri,
                },
                _ => unreachable!(),
            })
            .collect()
    }

    fn resolve_prefix(self, element: EvalNode<'a>, prefix: Option<&str>) -> Option<&'a str> {
        if let Self::Arena(arena) = self {
            if !arena.has_namespaces {
                return (prefix == Some("xml")).then_some(XML_NAMESPACE_URI);
            }
        }
        self.namespaces(element)
            .into_iter()
            .find_map(|namespace| match namespace {
                EvalNode::Namespace { namespace, .. } if namespace.prefix == prefix => {
                    (!namespace.uri.is_empty()).then_some(namespace.uri)
                }
                EvalNode::ArenaNamespace {
                    prefix: actual,
                    uri,
                    ..
                } if actual == prefix => (!uri.is_empty()).then_some(uri),
                _ => None,
            })
    }

    fn node_name(self, node: EvalNode<'a>) -> &'a str {
        match (self, node) {
            (_, EvalNode::Element(element)) => &element.name,
            (_, EvalNode::Attribute(attribute)) => &attribute.name,
            (_, EvalNode::ProcessingInstruction(pi)) => &pi.target,
            (_, EvalNode::Namespace { namespace, .. }) => namespace.prefix.unwrap_or_default(),
            (Self::Arena(arena), EvalNode::ArenaElement(index))
            | (Self::Arena(arena), EvalNode::ArenaProcessingInstruction(index)) => {
                &arena.nodes[index].primary
            }
            (Self::Arena(arena), EvalNode::ArenaAttribute(index)) => &arena.attributes[index].name,
            (_, EvalNode::ArenaNamespace { prefix, .. }) => prefix.unwrap_or_default(),
            _ => "",
        }
    }

    fn direct_value(self, node: EvalNode<'a>) -> &'a str {
        match (self, node) {
            (_, EvalNode::Attribute(attribute)) => &attribute.value,
            (_, EvalNode::Text(value) | EvalNode::Comment(value)) => value,
            (_, EvalNode::ProcessingInstruction(pi)) => &pi.data,
            (_, EvalNode::Namespace { namespace, .. }) => namespace.uri,
            (Self::Arena(arena), EvalNode::ArenaAttribute(index)) => &arena.attributes[index].value,
            (
                Self::Arena(arena),
                EvalNode::ArenaText(index)
                | EvalNode::ArenaComment(index)
                | EvalNode::ArenaProcessingInstruction(index),
            ) => {
                let node = &arena.nodes[index];
                if node.kind == XPathArenaNodeKind::ProcessingInstruction {
                    &arena.processing_instruction_values[node.secondary as usize]
                } else {
                    &node.primary
                }
            }
            (_, EvalNode::ArenaNamespace { uri, .. }) => uri,
            _ => "",
        }
    }

    fn text_content(self, node: EvalNode<'a>) -> String {
        if !matches!(node, EvalNode::Document) && !is_element_node(node) {
            return self.direct_value(node).to_owned();
        }
        if let Self::Arena(arena) = self {
            let (start, end) = match node {
                EvalNode::Document => (arena.root + 1, arena.nodes.len()),
                EvalNode::ArenaElement(index) => {
                    (index + 1, arena.nodes[index].subtree_end as usize)
                }
                _ => unreachable!("arena text content requires document or element"),
            };
            let mut output = String::new();
            for record in &arena.nodes[start..end] {
                if record.kind == XPathArenaNodeKind::Text {
                    output.push_str(&record.primary);
                }
            }
            return output;
        }
        let mut output = String::new();
        let root = if matches!(node, EvalNode::Document) {
            self.root()
        } else {
            node
        };
        let mut stack = self.children(root);
        stack.reverse();
        while let Some(candidate) = stack.pop() {
            match candidate {
                EvalNode::Text(_) | EvalNode::ArenaText(_) => {
                    output.push_str(self.direct_value(candidate));
                }
                _ if is_element_node(candidate) => {
                    let mut children = self.children(candidate);
                    children.reverse();
                    stack.extend(children);
                }
                _ => {}
            }
        }
        output
    }

    fn attribute_value(self, element: EvalNode<'a>, requested: &str) -> Option<&'a str> {
        self.attributes(element)
            .into_iter()
            .find(|attribute| self.node_name(*attribute) == requested)
            .map(|attribute| self.direct_value(attribute))
    }
}

fn arena_order_key<'a>(
    arena: &'a XPathArena<'a>,
    node: EvalNode<'a>,
) -> (usize, u8, usize, Option<&'a str>) {
    match node {
        EvalNode::Document => (0, 0, 0, None),
        EvalNode::ArenaElement(index)
        | EvalNode::ArenaText(index)
        | EvalNode::ArenaComment(index)
        | EvalNode::ArenaProcessingInstruction(index) => (index + 1, 0, 0, None),
        EvalNode::ArenaNamespace { owner, prefix, .. } => (owner + 1, 1, 0, prefix),
        EvalNode::ArenaAttribute(index) => {
            let attribute = &arena.attributes[index];
            let owner = &arena.nodes[attribute.owner];
            (
                attribute.owner + 1,
                2,
                index - owner.attribute_start as usize,
                None,
            )
        }
        _ => unreachable!("arena XPath order only receives arena nodes"),
    }
}

fn element_key(element: &XmlElement) -> usize {
    element as *const XmlElement as usize
}

fn attribute_key(attribute: &XmlAttribute) -> usize {
    attribute as *const XmlAttribute as usize
}

fn processing_instruction_key(pi: &XmlProcessingInstruction) -> usize {
    pi as *const XmlProcessingInstruction as usize
}

#[inline(never)]
fn append_element_order<'a>(element: &'a XmlElement, output: &mut Vec<EvalNode<'a>>) {
    output.push(EvalNode::Element(element));
    output.extend(element.attributes.iter().map(EvalNode::Attribute));
    let mut pending = Vec::new();
    let mut children = element.children.iter();
    let mut current = children.next();
    pending.extend(children.rev());
    while let Some(node) = current {
        current = None;
        match node {
            XmlNode::Element(element) => {
                output.push(EvalNode::Element(element));
                output.extend(element.attributes.iter().map(EvalNode::Attribute));
                let mut children = element.children.iter();
                current = children.next();
                pending.extend(children.rev());
            }
            _ => output.push(EvalNode::from(node)),
        }
        if current.is_none() {
            current = pending.pop();
        }
    }
}

fn append_arena_order<'a>(arena: &'a XPathArena<'a>, root: usize, output: &mut Vec<EvalNode<'a>>) {
    let mut stack = vec![root];
    while let Some(index) = stack.pop() {
        output.push(arena_eval_node(arena, index));
        let record = &arena.nodes[index];
        if record.kind == XPathArenaNodeKind::Element {
            output.extend(
                (record.attribute_start as usize
                    ..(record.attribute_start + record.attribute_count) as usize)
                    .map(EvalNode::ArenaAttribute),
            );
            let mut children = Vec::new();
            let mut child = expand_xpath_index(record.first_child);
            while let Some(index) = child {
                children.push(index);
                child = expand_xpath_index(arena.nodes[index].next_sibling);
            }
            stack.extend(children.into_iter().rev());
        }
    }
}

fn arena_eval_node<'a>(arena: &XPathArena<'a>, index: usize) -> EvalNode<'a> {
    match arena.nodes[index].kind {
        XPathArenaNodeKind::Element => EvalNode::ArenaElement(index),
        XPathArenaNodeKind::Text => EvalNode::ArenaText(index),
        XPathArenaNodeKind::Comment => EvalNode::ArenaComment(index),
        XPathArenaNodeKind::ProcessingInstruction => EvalNode::ArenaProcessingInstruction(index),
    }
}

fn is_namespace_declaration(name: &str) -> bool {
    name == "xmlns" || name.starts_with("xmlns:")
}

fn namespace_declaration_prefix(name: &str) -> Option<&str> {
    if name == "xmlns" {
        Some("")
    } else {
        name.strip_prefix("xmlns:")
    }
}

#[derive(Clone, Copy)]
enum EvalNode<'a> {
    Document,
    Element(&'a XmlElement),
    Attribute(&'a XmlAttribute),
    Text(&'a str),
    Comment(&'a str),
    ProcessingInstruction(&'a XmlProcessingInstruction),
    ArenaElement(usize),
    ArenaAttribute(usize),
    ArenaText(usize),
    ArenaComment(usize),
    ArenaProcessingInstruction(usize),
    Namespace {
        owner: &'a XmlElement,
        namespace: XmlNamespace<'a>,
    },
    ArenaNamespace {
        owner: usize,
        prefix: Option<&'a str>,
        uri: &'a str,
    },
}

impl<'a> From<&'a XmlNode> for EvalNode<'a> {
    fn from(node: &'a XmlNode) -> Self {
        match node {
            XmlNode::Element(element) => Self::Element(element),
            XmlNode::Text(value) | XmlNode::Cdata(value) => Self::Text(value),
            XmlNode::Comment(value) => Self::Comment(value),
            XmlNode::ProcessingInstruction(pi) => Self::ProcessingInstruction(pi),
        }
    }
}

impl<'a> EvalNode<'a> {
    fn key(self) -> EvalNodeKey<'a> {
        match self {
            Self::Document => EvalNodeKey::Document,
            Self::Element(element) => EvalNodeKey::Element(element_key(element)),
            Self::Attribute(attribute) => EvalNodeKey::Attribute(attribute_key(attribute)),
            Self::Text(value) => EvalNodeKey::Text {
                start: value.as_ptr() as usize,
                len: value.len(),
            },
            Self::Comment(value) => EvalNodeKey::Comment {
                start: value.as_ptr() as usize,
                len: value.len(),
            },
            Self::ProcessingInstruction(pi) => {
                EvalNodeKey::ProcessingInstruction(processing_instruction_key(pi))
            }
            Self::ArenaElement(index) => EvalNodeKey::ArenaElement(index),
            Self::ArenaAttribute(index) => EvalNodeKey::ArenaAttribute(index),
            Self::ArenaText(index) => EvalNodeKey::ArenaText(index),
            Self::ArenaComment(index) => EvalNodeKey::ArenaComment(index),
            Self::ArenaProcessingInstruction(index) => {
                EvalNodeKey::ArenaProcessingInstruction(index)
            }
            Self::Namespace { owner, namespace } => EvalNodeKey::Namespace {
                owner: element_key(owner),
                prefix: namespace.prefix,
            },
            Self::ArenaNamespace { owner, prefix, .. } => {
                EvalNodeKey::ArenaNamespace { owner, prefix }
            }
        }
    }

    fn public(self) -> Option<XPathNode<'a>> {
        match self {
            Self::Document => None,
            Self::Element(element) => Some(XPathNode::Element(element)),
            Self::Attribute(attribute) => Some(XPathNode::Attribute(attribute)),
            Self::Text(value) => Some(XPathNode::Text(value)),
            Self::Comment(value) => Some(XPathNode::Comment(value)),
            Self::ProcessingInstruction(pi) => Some(XPathNode::ProcessingInstruction(pi)),
            Self::Namespace { owner, namespace } => Some(XPathNode::Namespace { owner, namespace }),
            Self::ArenaElement(_)
            | Self::ArenaAttribute(_)
            | Self::ArenaText(_)
            | Self::ArenaComment(_)
            | Self::ArenaProcessingInstruction(_)
            | Self::ArenaNamespace { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum EvalNodeKey<'a> {
    Document,
    Element(usize),
    Attribute(usize),
    Text {
        start: usize,
        len: usize,
    },
    Comment {
        start: usize,
        len: usize,
    },
    ProcessingInstruction(usize),
    ArenaElement(usize),
    ArenaAttribute(usize),
    ArenaText(usize),
    ArenaComment(usize),
    ArenaProcessingInstruction(usize),
    Namespace {
        owner: usize,
        prefix: Option<&'a str>,
    },
    ArenaNamespace {
        owner: usize,
        prefix: Option<&'a str>,
    },
}

fn same_eval_node(left: EvalNode<'_>, right: EvalNode<'_>) -> bool {
    match (left, right) {
        (EvalNode::Document, EvalNode::Document) => true,
        (EvalNode::Element(left), EvalNode::Element(right)) => std::ptr::eq(left, right),
        (EvalNode::Attribute(left), EvalNode::Attribute(right)) => std::ptr::eq(left, right),
        (EvalNode::Text(left), EvalNode::Text(right))
        | (EvalNode::Comment(left), EvalNode::Comment(right)) => {
            left.as_ptr() == right.as_ptr() && left.len() == right.len()
        }
        (EvalNode::ProcessingInstruction(left), EvalNode::ProcessingInstruction(right)) => {
            std::ptr::eq(left, right)
        }
        (EvalNode::ArenaElement(left), EvalNode::ArenaElement(right))
        | (EvalNode::ArenaAttribute(left), EvalNode::ArenaAttribute(right))
        | (EvalNode::ArenaText(left), EvalNode::ArenaText(right))
        | (EvalNode::ArenaComment(left), EvalNode::ArenaComment(right))
        | (
            EvalNode::ArenaProcessingInstruction(left),
            EvalNode::ArenaProcessingInstruction(right),
        ) => left == right,
        (
            EvalNode::Namespace {
                owner: left_owner,
                namespace: left,
            },
            EvalNode::Namespace {
                owner: right_owner,
                namespace: right,
            },
        ) => std::ptr::eq(left_owner, right_owner) && left.prefix == right.prefix,
        (
            EvalNode::ArenaNamespace {
                owner: left_owner,
                prefix: left,
                ..
            },
            EvalNode::ArenaNamespace {
                owner: right_owner,
                prefix: right,
                ..
            },
        ) => left_owner == right_owner && left == right,
        _ => false,
    }
}

fn is_element_node(node: EvalNode<'_>) -> bool {
    matches!(node, EvalNode::Element(_) | EvalNode::ArenaElement(_))
}

fn is_attribute_node(node: EvalNode<'_>) -> bool {
    matches!(node, EvalNode::Attribute(_) | EvalNode::ArenaAttribute(_))
}

fn is_namespace_node(node: EvalNode<'_>) -> bool {
    matches!(
        node,
        EvalNode::Namespace { .. } | EvalNode::ArenaNamespace { .. }
    )
}

fn namespace_owner(node: EvalNode<'_>) -> Option<EvalNode<'_>> {
    match node {
        EvalNode::Namespace { owner, .. } => Some(EvalNode::Element(owner)),
        EvalNode::ArenaNamespace { owner, .. } => Some(EvalNode::ArenaElement(owner)),
        _ => None,
    }
}

fn element_eval_key(node: EvalNode<'_>) -> usize {
    match node {
        EvalNode::Element(element) => element_key(element),
        EvalNode::ArenaElement(index) => index,
        _ => unreachable!("element key requires element"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Axis {
    Child,
    Attribute,
    DescendantShorthand,
    Descendant,
    DescendantOrSelf,
    FollowingSibling,
    PrecedingSibling,
    Ancestor,
    AncestorOrSelf,
    Following,
    Preceding,
    SelfNode,
    Parent,
    Namespace,
    DescendantAttributeShorthand,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum NodeTest {
    Name { name: Option<String>, byte: usize },
    Text,
    Comment,
    ProcessingInstruction(Option<String>),
    Node,
}

#[derive(Clone, Debug)]
enum Predicate {
    Position(usize),
    Expression(ScalarExpr),
}

#[derive(Clone, Debug)]
struct Step {
    axis: Axis,
    test: NodeTest,
    predicates: Vec<Predicate>,
}

#[derive(Clone, Debug)]
struct LocationPath {
    absolute: bool,
    steps: Vec<Step>,
}

#[derive(Clone, Debug)]
struct Query {
    paths: Vec<LocationPath>,
}

#[derive(Clone, Debug)]
enum CompiledXPath {
    Nodes(Query),
    Scalar(ScalarExpr),
}

impl Query {
    fn parse(input: &str) -> Result<Self, XPathError> {
        validate_xpath_expression_depth(input)?;
        let parts = split_union(input)?;
        let mut paths = Vec::with_capacity(parts.len());
        for (offset, part) in parts {
            let mut path = PathParser::new(part).parse().map_err(|mut error| {
                error.byte += offset;
                error
            })?;
            path.shift_variable_bytes(offset);
            if path.steps.is_empty() {
                return Err(XPathError {
                    message: "empty XPath union branch",
                    byte: offset,
                });
            }
            paths.push(path);
        }
        Ok(Self { paths })
    }

    fn simple_descendant_filter(&self) -> Option<SimpleDescendantFilter> {
        let [path] = self.paths.as_slice() else {
            return None;
        };
        let [step] = path.steps.as_slice() else {
            return None;
        };
        if !path.absolute || step.axis != Axis::DescendantShorthand {
            return None;
        }
        let NodeTest::Name {
            name: element_name, ..
        } = &step.test
        else {
            return None;
        };
        if element_name.as_ref().is_some_and(|name| name.contains(':')) {
            return None;
        }
        let mut required_attributes = Vec::with_capacity(step.predicates.len());
        for predicate in &step.predicates {
            let Predicate::Expression(ScalarExpr::Path(attribute_path)) = predicate else {
                return None;
            };
            let [attribute_step] = attribute_path.steps.as_slice() else {
                return None;
            };
            let NodeTest::Name {
                name: Some(attribute),
                ..
            } = &attribute_step.test
            else {
                return None;
            };
            if attribute_path.absolute
                || attribute_step.axis != Axis::Attribute
                || !attribute_step.predicates.is_empty()
                || attribute.contains(':')
            {
                return None;
            }
            required_attributes.push(attribute.clone());
        }
        Some(SimpleDescendantFilter {
            element_name: element_name.clone(),
            required_attributes,
        })
    }

    fn evaluate<'a>(
        &self,
        tree: Tree<'a>,
        initial: EvalNode<'a>,
        variables: &XPathVariables,
        namespaces: &XPathNamespaces,
    ) -> Result<Vec<XPathNode<'a>>, XPathError> {
        Ok(tree
            .order(self.evaluate_raw(tree, initial, variables, namespaces)?)
            .into_iter()
            .filter_map(EvalNode::public)
            .collect())
    }

    fn evaluate_raw<'a>(
        &self,
        tree: Tree<'a>,
        initial: EvalNode<'a>,
        variables: &XPathVariables,
        namespaces: &XPathNamespaces,
    ) -> Result<Vec<EvalNode<'a>>, XPathError> {
        let mut union = Vec::new();
        for path in &self.paths {
            let contexts = if path.absolute {
                vec![EvalNode::Document]
            } else {
                vec![initial]
            };
            union.extend(path.evaluate(tree, contexts, variables, namespaces)?);
        }
        Ok(tree.order(union))
    }
}

impl LocationPath {
    fn evaluate<'a>(
        &self,
        tree: Tree<'a>,
        mut contexts: Vec<EvalNode<'a>>,
        variables: &XPathVariables,
        namespaces: &XPathNamespaces,
    ) -> Result<Vec<EvalNode<'a>>, XPathError> {
        for step in &self.steps {
            let mut selected = Vec::new();
            if step.axis == Axis::Following && step.predicates.is_empty() {
                if let Some(group) = tree.arena_following_union(&contexts) {
                    evaluate_step_group(tree, group, step, variables, namespaces, &mut selected)?;
                    contexts = tree.order(selected);
                    continue;
                }
            }
            if step.axis == Axis::Preceding && step.predicates.is_empty() {
                if let Some(group) = tree.arena_preceding_union(&contexts, false) {
                    evaluate_step_group(tree, group, step, variables, namespaces, &mut selected)?;
                    contexts = tree.order(selected);
                    continue;
                }
            }
            for context in contexts {
                if step.axis == Axis::DescendantShorthand {
                    if step.predicates.iter().all(predicate_is_context_boolean) {
                        evaluate_step_group(
                            tree,
                            tree.descendants(context),
                            step,
                            variables,
                            namespaces,
                            &mut selected,
                        )?;
                        continue;
                    }
                    let mut descendant_contexts = vec![context];
                    descendant_contexts.extend(tree.descendants(context));
                    for descendant in descendant_contexts {
                        evaluate_step_group(
                            tree,
                            tree.children(descendant),
                            step,
                            variables,
                            namespaces,
                            &mut selected,
                        )?;
                    }
                    continue;
                }
                if step.axis == Axis::DescendantAttributeShorthand {
                    let mut descendant_contexts = vec![context];
                    descendant_contexts.extend(tree.descendants(context));
                    for descendant in descendant_contexts {
                        evaluate_step_group(
                            tree,
                            tree.attributes(descendant),
                            step,
                            variables,
                            namespaces,
                            &mut selected,
                        )?;
                    }
                    continue;
                }
                for mut group in candidate_groups(tree, context, step) {
                    evaluate_step_group(
                        tree,
                        std::mem::take(&mut group),
                        step,
                        variables,
                        namespaces,
                        &mut selected,
                    )?;
                }
            }
            contexts = tree.order(selected);
        }
        Ok(contexts)
    }
}

fn predicate_is_context_boolean(predicate: &Predicate) -> bool {
    let Predicate::Expression(expression) = predicate else {
        return false;
    };
    scalar_is_context_boolean(expression)
}

fn scalar_is_context_boolean(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Path(_) => true,
        ScalarExpr::Compare(_, left, right) => {
            !scalar_uses_position_or_size(left) && !scalar_uses_position_or_size(right)
        }
        ScalarExpr::Or(left, right) | ScalarExpr::And(left, right) => {
            scalar_is_context_boolean(left) && scalar_is_context_boolean(right)
        }
        ScalarExpr::Function { function, .. } => matches!(
            function,
            CoreFunction::StartsWith
                | CoreFunction::Contains
                | CoreFunction::Boolean
                | CoreFunction::Not
                | CoreFunction::True
                | CoreFunction::False
                | CoreFunction::Lang
        ),
        _ => false,
    }
}

fn scalar_uses_position_or_size(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Or(left, right)
        | ScalarExpr::And(left, right)
        | ScalarExpr::Compare(_, left, right)
        | ScalarExpr::Arithmetic(_, left, right) => {
            scalar_uses_position_or_size(left) || scalar_uses_position_or_size(right)
        }
        ScalarExpr::Negate(value) => scalar_uses_position_or_size(value),
        ScalarExpr::Function {
            function,
            arguments,
            ..
        } => {
            matches!(function, CoreFunction::Position | CoreFunction::Last)
                || arguments.iter().any(scalar_uses_position_or_size)
        }
        ScalarExpr::Path(_)
        | ScalarExpr::String(_)
        | ScalarExpr::Number(_)
        | ScalarExpr::Variable { .. } => false,
    }
}

fn evaluate_step_group<'a>(
    tree: Tree<'a>,
    group: Vec<EvalNode<'a>>,
    step: &Step,
    variables: &XPathVariables,
    namespaces: &XPathNamespaces,
    selected: &mut Vec<EvalNode<'a>>,
) -> Result<(), XPathError> {
    let mut matched = Vec::with_capacity(group.len());
    for node in group {
        if matches_test(tree, node, &step.test, step.axis, namespaces)? {
            matched.push(node);
        }
    }
    for predicate in &step.predicates {
        apply_predicate(tree, &mut matched, predicate, variables, namespaces)?;
    }
    selected.extend(matched);
    Ok(())
}

fn candidate_groups<'a>(
    tree: Tree<'a>,
    context: EvalNode<'a>,
    step: &Step,
) -> Vec<Vec<EvalNode<'a>>> {
    match step.axis {
        Axis::Child => vec![tree.children(context)],
        Axis::Attribute => vec![tree.attributes(context)],
        Axis::DescendantAttributeShorthand => {
            unreachable!("descendant attribute shorthand is evaluated per descendant context")
        }
        Axis::Descendant => vec![tree.descendants(context)],
        Axis::DescendantOrSelf => {
            let mut nodes = vec![context];
            nodes.extend(tree.descendants(context));
            vec![nodes]
        }
        Axis::DescendantShorthand => {
            unreachable!("descendant shorthand is evaluated without materializing every group")
        }
        Axis::FollowingSibling => vec![tree.siblings(context, true)],
        Axis::PrecedingSibling => vec![tree.siblings(context, false)],
        Axis::Ancestor => vec![tree.ancestors(context)],
        Axis::AncestorOrSelf => {
            let mut nodes = vec![context];
            nodes.extend(tree.ancestors(context));
            vec![nodes]
        }
        Axis::Following => vec![tree.following(context)],
        Axis::Preceding => vec![tree.preceding(context)],
        Axis::SelfNode => vec![vec![context]],
        Axis::Parent => vec![tree.parent(context).into_iter().collect()],
        Axis::Namespace => {
            vec![if is_element_node(context) {
                tree.namespaces(context)
            } else {
                Vec::new()
            }]
        }
    }
}

fn matches_test(
    tree: Tree<'_>,
    node: EvalNode<'_>,
    test: &NodeTest,
    axis: Axis,
    namespaces: &XPathNamespaces,
) -> Result<bool, XPathError> {
    Ok(match test {
        NodeTest::Name { name: None, .. }
            if matches!(axis, Axis::Attribute | Axis::DescendantAttributeShorthand) =>
        {
            is_attribute_node(node)
        }
        NodeTest::Name { name: None, .. } if axis == Axis::Namespace => is_namespace_node(node),
        NodeTest::Name { name: None, .. } => is_element_node(node),
        NodeTest::Name {
            name: Some(name),
            byte,
        } => matches_expanded_name(tree, node, name, *byte, axis, namespaces)?,
        NodeTest::Text => matches!(node, EvalNode::Text(_) | EvalNode::ArenaText(_)),
        NodeTest::Comment => matches!(node, EvalNode::Comment(_) | EvalNode::ArenaComment(_)),
        NodeTest::ProcessingInstruction(target) => {
            matches!(
                node,
                EvalNode::ProcessingInstruction(_) | EvalNode::ArenaProcessingInstruction(_)
            ) && target
                .as_ref()
                .is_none_or(|target| tree.node_name(node) == target)
        }
        NodeTest::Node => true,
    })
}

fn matches_expanded_name(
    tree: Tree<'_>,
    node: EvalNode<'_>,
    requested: &str,
    byte: usize,
    axis: Axis,
    namespaces: &XPathNamespaces,
) -> Result<bool, XPathError> {
    if axis == Axis::Namespace {
        return Ok(is_namespace_node(node) && tree.node_name(node) == requested);
    }
    let (prefix, local) = if let Some(prefix) = requested.strip_suffix(":*") {
        (Some(prefix), None)
    } else {
        let name = XmlQualifiedName::parse(requested).map_err(|_| XPathError {
            message: "invalid XPath qualified name",
            byte,
        })?;
        (name.prefix, Some(name.local))
    };
    let requested_uri = match prefix {
        Some(prefix) => Some(namespaces.get(prefix).ok_or(XPathError {
            message: "unbound XPath namespace prefix",
            byte,
        })?),
        None => None,
    };

    let actual_name = tree.node_name(node);
    let actual_uri = if is_element_node(node) && axis != Axis::Attribute {
        let actual = XmlQualifiedName::parse(actual_name).ok();
        tree.resolve_prefix(node, actual.and_then(|name| name.prefix))
    } else if is_attribute_node(node)
        && matches!(axis, Axis::Attribute | Axis::DescendantAttributeShorthand)
    {
        let Some(owner) = tree.parent(node) else {
            return Ok(false);
        };
        XmlQualifiedName::parse(actual_name)
            .ok()
            .and_then(|name| name.prefix)
            .and_then(|prefix| tree.resolve_prefix(owner, Some(prefix)))
    } else {
        return Ok(false);
    };
    let Ok(actual) = XmlQualifiedName::parse(actual_name) else {
        return Ok(false);
    };
    Ok(local.is_none_or(|local| actual.local == local) && actual_uri == requested_uri)
}

fn apply_predicate(
    tree: Tree<'_>,
    nodes: &mut Vec<EvalNode<'_>>,
    predicate: &Predicate,
    variables: &XPathVariables,
    namespaces: &XPathNamespaces,
) -> Result<(), XPathError> {
    match predicate {
        Predicate::Position(position) => {
            if *position == 0 || *position > nodes.len() {
                nodes.clear();
            } else {
                let selected = nodes[*position - 1];
                nodes.clear();
                nodes.push(selected);
            }
        }
        Predicate::Expression(expression) => {
            let size = nodes.len();
            let mut position = 0usize;
            let mut retained = Vec::with_capacity(nodes.len());
            for node in nodes.iter().copied() {
                position += 1;
                let keep = if let ScalarExpr::Path(path) = expression {
                    if let Some(result) = simple_path_exists(tree, node, path, namespaces) {
                        result?
                    } else {
                        evaluate_predicate_scalar(
                            expression, tree, node, position, size, variables, namespaces,
                        )?
                    }
                } else {
                    evaluate_predicate_scalar(
                        expression, tree, node, position, size, variables, namespaces,
                    )?
                };
                if keep {
                    retained.push(node);
                }
            }
            *nodes = retained;
        }
    }
    Ok(())
}

fn evaluate_predicate_scalar(
    expression: &ScalarExpr,
    tree: Tree<'_>,
    node: EvalNode<'_>,
    position: usize,
    size: usize,
    variables: &XPathVariables,
    namespaces: &XPathNamespaces,
) -> Result<bool, XPathError> {
    Ok(
        match evaluate_scalar(
            expression,
            tree,
            node,
            position,
            size,
            (variables, namespaces),
        )? {
            ScalarValue::Number(number) => number == position as f64,
            value => value.into_bool(),
        },
    )
}

fn simple_path_exists(
    tree: Tree<'_>,
    context: EvalNode<'_>,
    path: &LocationPath,
    namespaces: &XPathNamespaces,
) -> Option<Result<bool, XPathError>> {
    if path.absolute || path.steps.len() != 1 {
        return None;
    }
    let step = &path.steps[0];
    if !step.predicates.is_empty() {
        return None;
    }
    if let Tree::Arena(arena) = tree {
        let result = match (step.axis, context) {
            (Axis::Attribute, EvalNode::ArenaElement(index)) => {
                let record = &arena.nodes[index];
                let range = record.attribute_start as usize
                    ..(record.attribute_start + record.attribute_count) as usize;
                let mut matched = false;
                for attribute in range {
                    if is_namespace_declaration(&arena.attributes[attribute].name) {
                        continue;
                    }
                    matched |= match matches_test(
                        tree,
                        EvalNode::ArenaAttribute(attribute),
                        &step.test,
                        step.axis,
                        namespaces,
                    ) {
                        Ok(value) => value,
                        Err(error) => return Some(Err(error)),
                    };
                    if matched {
                        break;
                    }
                }
                Some(matched)
            }
            (Axis::Child, EvalNode::ArenaElement(index)) => {
                let mut child = expand_xpath_index(arena.nodes[index].first_child);
                let mut matched = false;
                while let Some(index) = child {
                    matched |= match matches_test(
                        tree,
                        arena_eval_node(arena, index),
                        &step.test,
                        step.axis,
                        namespaces,
                    ) {
                        Ok(value) => value,
                        Err(error) => return Some(Err(error)),
                    };
                    if matched {
                        break;
                    }
                    child = expand_xpath_index(arena.nodes[index].next_sibling);
                }
                Some(matched)
            }
            _ => None,
        };
        if let Some(result) = result {
            return Some(Ok(result));
        }
    }
    let candidates = match step.axis {
        Axis::Child => tree.children(context),
        Axis::Attribute => tree.attributes(context),
        Axis::SelfNode => vec![context],
        Axis::Parent => tree.parent(context).into_iter().collect(),
        _ => return None,
    };
    Some(candidates.into_iter().try_fold(false, |matched, node| {
        Ok(matched || matches_test(tree, node, &step.test, step.axis, namespaces)?)
    }))
}

fn validate_xpath_expression_depth(input: &str) -> Result<(), XPathError> {
    if input.len() <= MAX_XPATH_EXPRESSION_DEPTH {
        return Ok(());
    }
    let mut depth = 0usize;
    let mut quote = None;
    for (index, byte) in input.bytes().enumerate() {
        if let Some(active) = quote {
            if byte == active {
                quote = None;
            }
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'(' | b'[' => {
                if depth >= MAX_XPATH_EXPRESSION_DEPTH {
                    return Err(xpath_expression_depth_error(index));
                }
                depth += 1;
            }
            b')' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

fn xpath_expression_depth_error(byte: usize) -> XPathError {
    XPathError {
        message: XPATH_EXPRESSION_DEPTH_ERROR,
        byte,
    }
}

fn split_union(input: &str) -> Result<Vec<(usize, &str)>, XPathError> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut brackets = 0usize;
    let mut parentheses = 0usize;
    let mut quote = None;
    for (index, byte) in input.bytes().enumerate() {
        match (quote, byte) {
            (Some(active), current) if current == active => quote = None,
            (Some(_), _) => {}
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'[') => brackets += 1,
            (None, b']') => brackets = brackets.saturating_sub(1),
            (None, b'(') => parentheses += 1,
            (None, b')') => parentheses = parentheses.saturating_sub(1),
            (None, b'|') if brackets == 0 && parentheses == 0 => {
                let raw = &input[start..index];
                let trimmed = raw.trim();
                let offset = start + raw.len() - raw.trim_start().len();
                if trimmed.is_empty() {
                    return Err(XPathError {
                        message: "empty XPath union branch",
                        byte: index,
                    });
                }
                parts.push((offset, trimmed));
                start = index + 1;
            }
            _ => {}
        }
    }
    let raw = &input[start..];
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(XPathError {
            message: "empty XPath expression",
            byte: start,
        });
    }
    parts.push((start + raw.len() - raw.trim_start().len(), trimmed));
    Ok(parts)
}

struct PathParser<'a> {
    input: &'a str,
    index: usize,
}

impl<'a> PathParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, index: 0 }
    }

    fn parse(mut self) -> Result<LocationPath, XPathError> {
        self.skip_whitespace();
        if self.is_eof() {
            return Err(self.error("empty XPath expression"));
        }

        let absolute = self.consume_byte(b'/');
        let mut default_axis = if absolute && self.consume_byte(b'/') {
            Axis::DescendantShorthand
        } else {
            Axis::Child
        };
        if absolute && self.is_eof() {
            return Err(self.error("document-node-only XPath is not supported"));
        }

        let mut steps = Vec::new();
        loop {
            self.skip_whitespace();
            let (axis, test) = self.parse_axis_and_test(default_axis)?;
            let mut predicates = Vec::new();
            loop {
                self.skip_whitespace();
                if !self.consume_byte(b'[') {
                    break;
                }
                predicates.push(self.parse_predicate()?);
            }
            steps.push(Step {
                axis,
                test,
                predicates,
            });

            self.skip_whitespace();
            if self.is_eof() {
                break;
            }
            if !self.consume_byte(b'/') {
                return Err(self.error("expected '/' between XPath steps"));
            }
            default_axis = if self.consume_byte(b'/') {
                Axis::DescendantShorthand
            } else {
                Axis::Child
            };
            if self.is_eof() {
                return Err(self.error("trailing XPath separator"));
            }
        }
        Ok(LocationPath { absolute, steps })
    }

    fn parse_axis_and_test(&mut self, default_axis: Axis) -> Result<(Axis, NodeTest), XPathError> {
        if self.consume_literal("..") {
            return Ok((Axis::Parent, NodeTest::Node));
        }
        if self.consume_byte(b'.') {
            return Ok((Axis::SelfNode, NodeTest::Node));
        }
        if self.consume_byte(b'@') {
            let axis = if default_axis == Axis::DescendantShorthand {
                Axis::DescendantAttributeShorthand
            } else {
                Axis::Attribute
            };
            return Ok((axis, self.parse_name_test()?));
        }

        let axis = [
            ("descendant-or-self::", Axis::DescendantOrSelf),
            ("following-sibling::", Axis::FollowingSibling),
            ("preceding-sibling::", Axis::PrecedingSibling),
            ("ancestor-or-self::", Axis::AncestorOrSelf),
            ("descendant::", Axis::Descendant),
            ("ancestor::", Axis::Ancestor),
            ("following::", Axis::Following),
            ("preceding::", Axis::Preceding),
            ("attribute::", Axis::Attribute),
            ("child::", Axis::Child),
            ("parent::", Axis::Parent),
            ("self::", Axis::SelfNode),
            ("namespace::", Axis::Namespace),
        ]
        .into_iter()
        .find_map(|(literal, axis)| self.consume_literal(literal).then_some(axis))
        .unwrap_or(default_axis);
        let axis = if default_axis == Axis::DescendantShorthand && axis == Axis::Attribute {
            Axis::DescendantAttributeShorthand
        } else {
            axis
        };
        Ok((axis, self.parse_node_test()?))
    }

    fn parse_node_test(&mut self) -> Result<NodeTest, XPathError> {
        if self.consume_literal("text()") {
            return Ok(NodeTest::Text);
        }
        if self.consume_literal("comment()") {
            return Ok(NodeTest::Comment);
        }
        if self.consume_literal("processing-instruction()") {
            return Ok(NodeTest::ProcessingInstruction(None));
        }
        for quote in ['\'', '"'] {
            let prefix = format!("processing-instruction({quote}");
            if self.consume_literal(&prefix) {
                let start = self.index;
                while self.peek().is_some_and(|byte| byte != quote as u8) {
                    self.index += 1;
                }
                let target = self.input[start..self.index].to_owned();
                if !self.consume_byte(quote as u8) || !self.consume_byte(b')') {
                    return Err(self.error("invalid processing-instruction() node test"));
                }
                return Ok(NodeTest::ProcessingInstruction(Some(target)));
            }
        }
        if self.consume_literal("node()") {
            return Ok(NodeTest::Node);
        }
        self.parse_name_test()
    }

    fn parse_name_test(&mut self) -> Result<NodeTest, XPathError> {
        let start = self.index;
        if self.consume_byte(b'*') {
            return Ok(NodeTest::Name {
                name: None,
                byte: start,
            });
        }
        let start = self.index;
        let mut characters = self.input[self.index..].char_indices();
        let Some((_, first)) = characters.next() else {
            return Err(self.error("expected XPath name test"));
        };
        if !crate::syntax::is_name_start_char(first) {
            return Err(self.error("expected XPath name test"));
        }
        self.index += first.len_utf8();
        for (_, character) in characters {
            if !crate::syntax::is_name_char(character) {
                break;
            }
            self.index += character.len_utf8();
        }
        if self.peek() == Some(b'*')
            && self.input.as_bytes().get(self.index.wrapping_sub(1)) == Some(&b':')
        {
            self.index += 1;
        }
        let name = self.input[start..self.index].to_owned();
        if !name.ends_with(":*") {
            crate::XmlQualifiedName::parse(&name)
                .map_err(|_| self.error("invalid XPath qualified name"))?;
        }
        Ok(NodeTest::Name {
            name: Some(name),
            byte: start,
        })
    }

    fn parse_predicate(&mut self) -> Result<Predicate, XPathError> {
        self.skip_whitespace();
        let start = self.index;
        let mut quote = None;
        let mut nested_brackets = 0usize;
        while let Some(byte) = self.peek() {
            match (quote, byte) {
                (Some(active), current) if current == active => quote = None,
                (None, b'\'' | b'"') => quote = Some(byte),
                (None, b'[') => nested_brackets += 1,
                (None, b']') if nested_brackets > 0 => nested_brackets -= 1,
                (None, b']') => break,
                _ => {}
            }
            self.index += 1;
        }
        if quote.is_some() || !self.consume_byte(b']') {
            return Err(self.error("unterminated XPath predicate"));
        }
        let untrimmed = &self.input[start..self.index - 1];
        let raw = untrimmed.trim();
        let offset = start + untrimmed.len() - untrimmed.trim_start().len();
        if let Ok(position) = raw.parse::<usize>() {
            return Ok(Predicate::Position(position));
        }
        let mut expression = PredicateParser::new(raw).parse().map_err(|mut error| {
            error.byte += offset;
            error
        })?;
        expression.shift_variable_bytes(offset);
        Ok(Predicate::Expression(expression))
    }

    fn consume_literal(&mut self, literal: &str) -> bool {
        if self.input[self.index..].starts_with(literal) {
            self.index += literal.len();
            true
        } else {
            false
        }
    }

    fn consume_byte(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.index += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.as_bytes().get(self.index).copied()
    }

    fn is_eof(&self) -> bool {
        self.index == self.input.len()
    }

    fn error(&self, message: &'static str) -> XPathError {
        XPathError {
            message,
            byte: self.index,
        }
    }
}

#[derive(Clone, Debug)]
enum ScalarExpr {
    Or(Box<Self>, Box<Self>),
    And(Box<Self>, Box<Self>),
    Compare(Comparison, Box<Self>, Box<Self>),
    Arithmetic(Arithmetic, Box<Self>, Box<Self>),
    Negate(Box<Self>),
    String(String),
    Number(f64),
    Variable {
        name: String,
        byte: usize,
    },
    Path(LocationPath),
    Function {
        function: CoreFunction,
        arguments: Vec<Self>,
        byte: usize,
    },
}

struct ParsedScalar {
    expression: ScalarExpr,
    depth: usize,
}

impl ParsedScalar {
    fn leaf(expression: ScalarExpr) -> Self {
        Self {
            expression,
            depth: 1,
        }
    }

    fn unary<const CHECK_DEPTH: bool>(
        value: Self,
        byte: usize,
        build: impl FnOnce(Box<ScalarExpr>) -> ScalarExpr,
    ) -> Result<Self, XPathError> {
        let depth = if CHECK_DEPTH { value.depth + 1 } else { 0 };
        if CHECK_DEPTH && depth > MAX_XPATH_EXPRESSION_DEPTH {
            return Err(xpath_expression_depth_error(byte));
        }
        Ok(Self {
            expression: build(Box::new(value.expression)),
            depth,
        })
    }

    fn binary<const CHECK_DEPTH: bool>(
        left: Self,
        right: Self,
        byte: usize,
        build: impl FnOnce(Box<ScalarExpr>, Box<ScalarExpr>) -> ScalarExpr,
    ) -> Result<Self, XPathError> {
        let depth = if CHECK_DEPTH {
            left.depth.max(right.depth) + 1
        } else {
            0
        };
        if CHECK_DEPTH && depth > MAX_XPATH_EXPRESSION_DEPTH {
            return Err(xpath_expression_depth_error(byte));
        }
        Ok(Self {
            expression: build(Box::new(left.expression), Box::new(right.expression)),
            depth,
        })
    }

    fn function<const CHECK_DEPTH: bool>(
        function: CoreFunction,
        arguments: Vec<Self>,
        byte: usize,
    ) -> Result<Self, XPathError> {
        let depth = if CHECK_DEPTH {
            arguments
                .iter()
                .map(|argument| argument.depth)
                .max()
                .unwrap_or(0)
                + 1
        } else {
            0
        };
        if CHECK_DEPTH && depth > MAX_XPATH_EXPRESSION_DEPTH {
            return Err(xpath_expression_depth_error(byte));
        }
        Ok(Self {
            expression: ScalarExpr::Function {
                function,
                arguments: arguments
                    .into_iter()
                    .map(|argument| argument.expression)
                    .collect(),
                byte,
            },
            depth,
        })
    }
}

impl ScalarExpr {
    fn shift_variable_bytes(&mut self, offset: usize) {
        match self {
            Self::Or(left, right)
            | Self::And(left, right)
            | Self::Compare(_, left, right)
            | Self::Arithmetic(_, left, right) => {
                left.shift_variable_bytes(offset);
                right.shift_variable_bytes(offset);
            }
            Self::Variable { byte, .. } => *byte += offset,
            Self::Path(path) => path.shift_variable_bytes(offset),
            Self::Negate(value) => {
                value.shift_variable_bytes(offset);
            }
            Self::Function {
                arguments, byte, ..
            } => {
                *byte += offset;
                for argument in arguments {
                    argument.shift_variable_bytes(offset);
                }
            }
            Self::String(_) | Self::Number(_) => {}
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Arithmetic {
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
}

#[derive(Clone, Copy, Debug)]
enum CoreFunction {
    Last,
    Position,
    Count,
    Id,
    LocalName,
    NamespaceUri,
    Name,
    String,
    Concat,
    StartsWith,
    Contains,
    SubstringBefore,
    SubstringAfter,
    Substring,
    StringLength,
    NormalizeSpace,
    Translate,
    Boolean,
    Not,
    True,
    False,
    Lang,
    Number,
    Sum,
    Floor,
    Ceiling,
    Round,
}

impl CoreFunction {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "last" => Self::Last,
            "position" => Self::Position,
            "count" => Self::Count,
            "id" => Self::Id,
            "local-name" => Self::LocalName,
            "namespace-uri" => Self::NamespaceUri,
            "name" => Self::Name,
            "string" => Self::String,
            "concat" => Self::Concat,
            "starts-with" => Self::StartsWith,
            "contains" => Self::Contains,
            "substring-before" => Self::SubstringBefore,
            "substring-after" => Self::SubstringAfter,
            "substring" => Self::Substring,
            "string-length" => Self::StringLength,
            "normalize-space" => Self::NormalizeSpace,
            "translate" => Self::Translate,
            "boolean" => Self::Boolean,
            "not" => Self::Not,
            "true" => Self::True,
            "false" => Self::False,
            "lang" => Self::Lang,
            "number" => Self::Number,
            "sum" => Self::Sum,
            "floor" => Self::Floor,
            "ceiling" => Self::Ceiling,
            "round" => Self::Round,
            _ => return None,
        })
    }

    fn accepts(self, count: usize) -> bool {
        match self {
            Self::Last | Self::Position | Self::True | Self::False => count == 0,
            Self::Count
            | Self::Id
            | Self::Boolean
            | Self::Not
            | Self::Lang
            | Self::Sum
            | Self::Floor
            | Self::Ceiling
            | Self::Round => count == 1,
            Self::LocalName
            | Self::NamespaceUri
            | Self::Name
            | Self::String
            | Self::StringLength
            | Self::NormalizeSpace
            | Self::Number => count <= 1,
            Self::StartsWith | Self::Contains | Self::SubstringBefore | Self::SubstringAfter => {
                count == 2
            }
            Self::Substring => matches!(count, 2 | 3),
            Self::Translate => count == 3,
            Self::Concat => count >= 2,
        }
    }
}

impl LocationPath {
    fn shift_variable_bytes(&mut self, offset: usize) {
        for predicate in self
            .steps
            .iter_mut()
            .flat_map(|step| step.predicates.iter_mut())
        {
            if let Predicate::Expression(expression) = predicate {
                expression.shift_variable_bytes(offset);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Comparison {
    Equal,
    NotEqual,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

#[derive(Clone)]
enum ScalarValue<'a> {
    Nodes(Vec<EvalNode<'a>>),
    Bool(bool),
    Number(f64),
    String(String),
}

impl ScalarValue<'_> {
    fn into_bool(self) -> bool {
        match self {
            Self::Nodes(nodes) => !nodes.is_empty(),
            Self::Bool(value) => value,
            Self::Number(value) => value != 0.0 && !value.is_nan(),
            Self::String(value) => !value.is_empty(),
        }
    }

    fn into_string(self, tree: Tree<'_>) -> String {
        match self {
            Self::Nodes(nodes) => nodes
                .first()
                .map_or_else(String::new, |node| node_string(tree, *node)),
            Self::Bool(true) => "true".to_owned(),
            Self::Bool(false) => "false".to_owned(),
            Self::Number(value) => format_xpath_number(value),
            Self::String(value) => value,
        }
    }

    fn into_number(self, tree: Tree<'_>) -> f64 {
        match self {
            Self::Number(value) => value,
            Self::Bool(true) => 1.0,
            Self::Bool(false) => 0.0,
            value => value.into_string(tree).trim().parse().unwrap_or(f64::NAN),
        }
    }
}

fn evaluate_scalar<'a>(
    expression: &ScalarExpr,
    tree: Tree<'a>,
    context: EvalNode<'a>,
    position: usize,
    size: usize,
    bindings: (&XPathVariables, &XPathNamespaces),
) -> Result<ScalarValue<'a>, XPathError> {
    let (variables, namespaces) = bindings;
    Ok(match expression {
        ScalarExpr::Or(left, right) => {
            let left = evaluate_scalar(left, tree, context, position, size, bindings)?.into_bool();
            ScalarValue::Bool(
                left || evaluate_scalar(right, tree, context, position, size, bindings)?
                    .into_bool(),
            )
        }
        ScalarExpr::And(left, right) => {
            let left = evaluate_scalar(left, tree, context, position, size, bindings)?.into_bool();
            ScalarValue::Bool(
                left && evaluate_scalar(right, tree, context, position, size, bindings)?
                    .into_bool(),
            )
        }
        ScalarExpr::Compare(comparison, left, right) => ScalarValue::Bool(compare_values(
            evaluate_scalar(left, tree, context, position, size, bindings)?,
            evaluate_scalar(right, tree, context, position, size, bindings)?,
            *comparison,
            tree,
        )),
        ScalarExpr::Arithmetic(operation, left, right) => {
            let left =
                evaluate_scalar(left, tree, context, position, size, bindings)?.into_number(tree);
            let right =
                evaluate_scalar(right, tree, context, position, size, bindings)?.into_number(tree);
            ScalarValue::Number(match operation {
                Arithmetic::Add => left + right,
                Arithmetic::Subtract => left - right,
                Arithmetic::Multiply => left * right,
                Arithmetic::Divide => left / right,
                Arithmetic::Modulo => left % right,
            })
        }
        ScalarExpr::Negate(value) => ScalarValue::Number(
            -evaluate_scalar(value, tree, context, position, size, bindings)?.into_number(tree),
        ),
        ScalarExpr::String(value) => ScalarValue::String(value.clone()),
        ScalarExpr::Number(value) => ScalarValue::Number(*value),
        ScalarExpr::Variable { name, byte } => match variables.get(name) {
            Some(XPathVariable::Boolean(value)) => ScalarValue::Bool(*value),
            Some(XPathVariable::Number(value)) => ScalarValue::Number(*value),
            Some(XPathVariable::String(value)) => ScalarValue::String(value.clone()),
            None => {
                return Err(XPathError {
                    message: "undefined XPath variable",
                    byte: *byte,
                });
            }
        },
        ScalarExpr::Path(path) => ScalarValue::Nodes(path.evaluate(
            tree,
            if path.absolute {
                vec![EvalNode::Document]
            } else {
                vec![context]
            },
            variables,
            namespaces,
        )?),
        ScalarExpr::Function {
            function,
            arguments,
            byte,
        } => evaluate_function(
            *function,
            arguments,
            *byte,
            ScalarContext {
                tree,
                context,
                position,
                size,
                variables,
                namespaces,
            },
        )?,
    })
}

#[derive(Clone, Copy)]
struct ScalarContext<'a, 'variables> {
    tree: Tree<'a>,
    context: EvalNode<'a>,
    position: usize,
    size: usize,
    variables: &'variables XPathVariables,
    namespaces: &'variables XPathNamespaces,
}

fn evaluate_function<'a>(
    function: CoreFunction,
    arguments: &[ScalarExpr],
    byte: usize,
    evaluation: ScalarContext<'a, '_>,
) -> Result<ScalarValue<'a>, XPathError> {
    let ScalarContext {
        tree,
        context,
        position,
        size,
        variables,
        namespaces,
    } = evaluation;
    let evaluate = |argument: &ScalarExpr| {
        evaluate_scalar(
            argument,
            tree,
            context,
            position,
            size,
            (variables, namespaces),
        )
    };
    let context_value = || ScalarValue::Nodes(vec![context]);
    let type_error = || XPathError {
        message: "invalid XPath function argument type",
        byte,
    };

    Ok(match function {
        CoreFunction::Last => ScalarValue::Number(size as f64),
        CoreFunction::Position => ScalarValue::Number(position as f64),
        CoreFunction::Count => match evaluate(&arguments[0])? {
            ScalarValue::Nodes(nodes) => ScalarValue::Number(nodes.len() as f64),
            _ => return Err(type_error()),
        },
        CoreFunction::Id => {
            let value = evaluate(&arguments[0])?;
            let ids: Vec<String> = match value {
                ScalarValue::Nodes(nodes) => nodes
                    .into_iter()
                    .flat_map(|node| {
                        node_string(tree, node)
                            .split_ascii_whitespace()
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .collect(),
                value => value
                    .into_string(tree)
                    .split_ascii_whitespace()
                    .map(str::to_owned)
                    .collect(),
            };
            ScalarValue::Nodes(
                tree.document_order()
                    .into_iter()
                    .filter(|node| {
                        is_element_node(*node)
                            && tree.attributes(*node).into_iter().any(|attribute| {
                                matches!(tree.node_name(attribute), "id" | "xml:id")
                                    && ids.iter().any(|id| id == tree.direct_value(attribute))
                            })
                    })
                    .collect(),
            )
        }
        CoreFunction::LocalName | CoreFunction::Name | CoreFunction::NamespaceUri => {
            let value = if arguments.is_empty() {
                context_value()
            } else {
                evaluate(&arguments[0])?
            };
            let ScalarValue::Nodes(nodes) = value else {
                return Err(type_error());
            };
            if matches!(function, CoreFunction::NamespaceUri) {
                return Ok(ScalarValue::String(
                    nodes
                        .first()
                        .and_then(|node| node_namespace_uri(tree, *node))
                        .unwrap_or_default()
                        .to_owned(),
                ));
            }
            let name = nodes.first().map_or("", |node| tree.node_name(*node));
            ScalarValue::String(
                match function {
                    CoreFunction::LocalName => {
                        name.rsplit_once(':').map_or(name, |(_, local)| local)
                    }
                    CoreFunction::Name => name,
                    CoreFunction::NamespaceUri => unreachable!(),
                    _ => unreachable!(),
                }
                .to_owned(),
            )
        }
        CoreFunction::String => ScalarValue::String(
            if arguments.is_empty() {
                context_value()
            } else {
                evaluate(&arguments[0])?
            }
            .into_string(tree),
        ),
        CoreFunction::Concat => {
            let mut output = String::new();
            for argument in arguments {
                output.push_str(&evaluate(argument)?.into_string(tree));
            }
            ScalarValue::String(output)
        }
        CoreFunction::StartsWith | CoreFunction::Contains => {
            let left = evaluate(&arguments[0])?.into_string(tree);
            let right = evaluate(&arguments[1])?.into_string(tree);
            ScalarValue::Bool(match function {
                CoreFunction::StartsWith => left.starts_with(&right),
                CoreFunction::Contains => left.contains(&right),
                _ => unreachable!(),
            })
        }
        CoreFunction::SubstringBefore | CoreFunction::SubstringAfter => {
            let value = evaluate(&arguments[0])?.into_string(tree);
            let needle = evaluate(&arguments[1])?.into_string(tree);
            let found = value.find(&needle);
            ScalarValue::String(match (function, found) {
                (_, None) => String::new(),
                (CoreFunction::SubstringBefore, Some(index)) => value[..index].to_owned(),
                (CoreFunction::SubstringAfter, Some(index)) => {
                    value[index + needle.len()..].to_owned()
                }
                _ => unreachable!(),
            })
        }
        CoreFunction::Substring => {
            let value = evaluate(&arguments[0])?.into_string(tree);
            let start = xpath_round(evaluate(&arguments[1])?.into_number(tree));
            let end = if arguments.len() == 3 {
                start + xpath_round(evaluate(&arguments[2])?.into_number(tree))
            } else {
                f64::INFINITY
            };
            let output = value
                .chars()
                .enumerate()
                .filter_map(|(index, character)| {
                    let xpath_position = (index + 1) as f64;
                    (xpath_position >= start && xpath_position < end).then_some(character)
                })
                .collect();
            ScalarValue::String(output)
        }
        CoreFunction::StringLength => {
            let value = if arguments.is_empty() {
                context_value()
            } else {
                evaluate(&arguments[0])?
            };
            ScalarValue::Number(value.into_string(tree).chars().count() as f64)
        }
        CoreFunction::NormalizeSpace => {
            let value = if arguments.is_empty() {
                context_value()
            } else {
                evaluate(&arguments[0])?
            };
            ScalarValue::String(
                value
                    .into_string(tree)
                    .split_ascii_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        }
        CoreFunction::Translate => {
            let value = evaluate(&arguments[0])?.into_string(tree);
            let from: Vec<_> = evaluate(&arguments[1])?.into_string(tree).chars().collect();
            let to: Vec<_> = evaluate(&arguments[2])?.into_string(tree).chars().collect();
            ScalarValue::String(
                value
                    .chars()
                    .filter_map(|character| {
                        from.iter()
                            .position(|candidate| *candidate == character)
                            .map_or(Some(character), |index| to.get(index).copied())
                    })
                    .collect(),
            )
        }
        CoreFunction::Boolean => ScalarValue::Bool(evaluate(&arguments[0])?.into_bool()),
        CoreFunction::Not => ScalarValue::Bool(!evaluate(&arguments[0])?.into_bool()),
        CoreFunction::True => ScalarValue::Bool(true),
        CoreFunction::False => ScalarValue::Bool(false),
        CoreFunction::Lang => {
            let requested = evaluate(&arguments[0])?
                .into_string(tree)
                .to_ascii_lowercase();
            let actual = context_language(tree, context)
                .unwrap_or_default()
                .to_ascii_lowercase();
            ScalarValue::Bool(
                actual == requested
                    || actual
                        .strip_prefix(&requested)
                        .is_some_and(|suffix| suffix.starts_with('-')),
            )
        }
        CoreFunction::Number => ScalarValue::Number(
            if arguments.is_empty() {
                context_value()
            } else {
                evaluate(&arguments[0])?
            }
            .into_number(tree),
        ),
        CoreFunction::Sum => match evaluate(&arguments[0])? {
            ScalarValue::Nodes(nodes) => ScalarValue::Number(
                nodes
                    .into_iter()
                    .map(|node| {
                        node_string(tree, node)
                            .trim()
                            .parse::<f64>()
                            .unwrap_or(f64::NAN)
                    })
                    .sum(),
            ),
            _ => return Err(type_error()),
        },
        CoreFunction::Floor | CoreFunction::Ceiling | CoreFunction::Round => {
            let value = evaluate(&arguments[0])?.into_number(tree);
            ScalarValue::Number(match function {
                CoreFunction::Floor => value.floor(),
                CoreFunction::Ceiling => value.ceil(),
                CoreFunction::Round => xpath_round(value),
                _ => unreachable!(),
            })
        }
    })
}

fn node_namespace_uri<'a>(tree: Tree<'a>, node: EvalNode<'a>) -> Option<&'a str> {
    if is_element_node(node) {
        let prefix = XmlQualifiedName::parse(tree.node_name(node))
            .ok()
            .and_then(|name| name.prefix);
        tree.resolve_prefix(node, prefix)
    } else if is_attribute_node(node) {
        let owner = tree.parent(node)?;
        XmlQualifiedName::parse(tree.node_name(node))
            .ok()
            .and_then(|name| name.prefix)
            .and_then(|prefix| tree.resolve_prefix(owner, Some(prefix)))
    } else {
        None
    }
}

fn context_language<'a>(tree: Tree<'a>, context: EvalNode<'a>) -> Option<&'a str> {
    let mut current = match context {
        node if is_element_node(node) => Some(context),
        _ => tree.parent(context),
    };
    while let Some(element) = current.filter(|node| is_element_node(*node)) {
        if let Some(language) = tree.attribute_value(element, "xml:lang") {
            return Some(language);
        }
        current = tree.parent(element);
    }
    None
}

fn xpath_round(value: f64) -> f64 {
    if (-0.5..0.0).contains(&value) {
        -0.0
    } else {
        (value + 0.5).floor()
    }
}

fn compare_values(
    left: ScalarValue<'_>,
    right: ScalarValue<'_>,
    comparison: Comparison,
    tree: Tree<'_>,
) -> bool {
    let (left, right) = match (left, right) {
        (ScalarValue::Nodes(left), ScalarValue::Nodes(right)) => {
            if left.is_empty() || right.is_empty() {
                return false;
            }
            let first_left = node_string(tree, left[0]);
            let first_right = node_string(tree, right[0]);
            if left.len() == 1 && right.len() == 1 {
                return compare_strings(&first_left, &first_right, comparison);
            }
            match comparison {
                Comparison::Equal if first_left == first_right => return true,
                Comparison::NotEqual if first_left != first_right => return true,
                Comparison::Less
                | Comparison::LessOrEqual
                | Comparison::Greater
                | Comparison::GreaterOrEqual
                    if compare_strings(&first_left, &first_right, comparison) =>
                {
                    return true;
                }
                _ => {}
            }
            return compare_node_sets(&left, &right, comparison, tree, first_left, first_right);
        }
        (ScalarValue::Nodes(nodes), ScalarValue::Number(number)) => {
            return nodes.into_iter().any(|node| {
                compare_numbers(
                    node_string(tree, node).trim().parse().unwrap_or(f64::NAN),
                    number,
                    comparison,
                )
            });
        }
        (ScalarValue::Number(number), ScalarValue::Nodes(nodes)) => {
            return nodes.into_iter().any(|node| {
                compare_numbers(
                    number,
                    node_string(tree, node).trim().parse().unwrap_or(f64::NAN),
                    comparison,
                )
            });
        }
        (ScalarValue::Nodes(nodes), ScalarValue::Bool(value)) => {
            return compare_boole(!nodes.is_empty(), value, comparison);
        }
        (ScalarValue::Bool(value), ScalarValue::Nodes(nodes)) => {
            return compare_boole(value, !nodes.is_empty(), comparison);
        }
        (ScalarValue::Nodes(nodes), value) => {
            let right = value.into_string(tree);
            return nodes
                .into_iter()
                .any(|node| compare_strings(&node_string(tree, node), &right, comparison));
        }
        (value, ScalarValue::Nodes(nodes)) => {
            let left = value.into_string(tree);
            return nodes
                .into_iter()
                .any(|node| compare_strings(&left, &node_string(tree, node), comparison));
        }
        values => values,
    };
    match comparison {
        Comparison::Equal | Comparison::NotEqual => {
            let equal = match (&left, &right) {
                (ScalarValue::Bool(_), _) | (_, ScalarValue::Bool(_)) => {
                    left.into_bool() == right.into_bool()
                }
                (ScalarValue::Number(_), _) | (_, ScalarValue::Number(_)) => {
                    left.into_number(tree) == right.into_number(tree)
                }
                _ => left.into_string(tree) == right.into_string(tree),
            };
            if comparison == Comparison::Equal {
                equal
            } else {
                !equal
            }
        }
        _ => compare_numbers(left.into_number(tree), right.into_number(tree), comparison),
    }
}

#[inline(never)]
fn compare_node_sets(
    left: &[EvalNode<'_>],
    right: &[EvalNode<'_>],
    comparison: Comparison,
    tree: Tree<'_>,
    first_left: String,
    first_right: String,
) -> bool {
    match comparison {
        Comparison::Equal => {
            node_sets_have_equal_string(left, right, tree, first_left, first_right)
        }
        Comparison::NotEqual => node_sets_have_unequal_string(left, right, tree, first_left),
        comparison => {
            let first_left = string_number(&first_left);
            let first_right = string_number(&first_right);
            let (left_min, left_max) = node_set_number_bounds(first_left, &left[1..], tree);
            let (right_min, right_max) = node_set_number_bounds(first_right, &right[1..], tree);
            match comparison {
                Comparison::Less => left_min.is_some_and(|left| {
                    right_max.is_some_and(|right| compare_numbers(left, right, comparison))
                }),
                Comparison::LessOrEqual => left_min.is_some_and(|left| {
                    right_max.is_some_and(|right| compare_numbers(left, right, comparison))
                }),
                Comparison::Greater => left_max.is_some_and(|left| {
                    right_min.is_some_and(|right| compare_numbers(left, right, comparison))
                }),
                Comparison::GreaterOrEqual => left_max.is_some_and(|left| {
                    right_min.is_some_and(|right| compare_numbers(left, right, comparison))
                }),
                Comparison::Equal | Comparison::NotEqual => unreachable!(),
            }
        }
    }
}

fn node_sets_have_equal_string(
    left: &[EvalNode<'_>],
    right: &[EvalNode<'_>],
    tree: Tree<'_>,
    first_left: String,
    first_right: String,
) -> bool {
    if left.len() <= right.len() {
        let mut values = HashSet::with_capacity(left.len());
        values.insert(first_left);
        values.extend(left[1..].iter().map(|node| node_string(tree, *node)));
        values.contains(first_right.as_str())
            || right[1..]
                .iter()
                .any(|node| values.contains(node_string(tree, *node).as_str()))
    } else {
        let mut values = HashSet::with_capacity(right.len());
        values.insert(first_right);
        values.extend(right[1..].iter().map(|node| node_string(tree, *node)));
        values.contains(first_left.as_str())
            || left[1..]
                .iter()
                .any(|node| values.contains(node_string(tree, *node).as_str()))
    }
}

fn node_sets_have_unequal_string(
    left: &[EvalNode<'_>],
    right: &[EvalNode<'_>],
    tree: Tree<'_>,
    reference: String,
) -> bool {
    left[1..]
        .iter()
        .chain(&right[1..])
        .any(|node| node_string(tree, *node) != reference)
}

fn node_set_number_bounds(
    first: f64,
    nodes: &[EvalNode<'_>],
    tree: Tree<'_>,
) -> (Option<f64>, Option<f64>) {
    let mut minimum = (!first.is_nan()).then_some(first);
    let mut maximum = minimum;
    for node in nodes {
        let value = node_number(tree, *node);
        if value.is_nan() {
            continue;
        }
        minimum = Some(minimum.map_or(value, |current| current.min(value)));
        maximum = Some(maximum.map_or(value, |current| current.max(value)));
    }
    (minimum, maximum)
}

fn node_number(tree: Tree<'_>, node: EvalNode<'_>) -> f64 {
    string_number(&node_string(tree, node))
}

fn string_number(value: &str) -> f64 {
    value.trim().parse::<f64>().unwrap_or(f64::NAN)
}

fn compare_boole(left: bool, right: bool, comparison: Comparison) -> bool {
    match comparison {
        Comparison::Equal => left == right,
        Comparison::NotEqual => left != right,
        _ => compare_numbers(
            usize::from(left) as f64,
            usize::from(right) as f64,
            comparison,
        ),
    }
}

fn compare_strings(left: &str, right: &str, comparison: Comparison) -> bool {
    match comparison {
        Comparison::Equal => left == right,
        Comparison::NotEqual => left != right,
        _ => compare_numbers(
            left.trim().parse().unwrap_or(f64::NAN),
            right.trim().parse().unwrap_or(f64::NAN),
            comparison,
        ),
    }
}

fn compare_numbers(left: f64, right: f64, comparison: Comparison) -> bool {
    match comparison {
        Comparison::Equal => left == right,
        Comparison::NotEqual => left != right,
        Comparison::Less => left < right,
        Comparison::LessOrEqual => left <= right,
        Comparison::Greater => left > right,
        Comparison::GreaterOrEqual => left >= right,
    }
}

fn node_string(tree: Tree<'_>, node: EvalNode<'_>) -> String {
    match node {
        EvalNode::Document | EvalNode::Element(_) | EvalNode::ArenaElement(_) => {
            tree.text_content(node)
        }
        _ => tree.direct_value(node).to_owned(),
    }
}

fn format_xpath_number(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else if value == 0.0 {
        "0".to_owned()
    } else {
        value.to_string()
    }
}

struct PredicateParser<'a> {
    input: &'a str,
    index: usize,
}

impl<'a> PredicateParser<'a> {
    #[inline(always)]
    fn new(input: &'a str) -> Self {
        Self { input, index: 0 }
    }

    #[inline(always)]
    fn parse(self) -> Result<ScalarExpr, XPathError> {
        if self.input.len() > MAX_XPATH_EXPRESSION_DEPTH {
            self.parse_with_depth_limit::<true>()
        } else {
            self.parse_fast()
        }
    }

    #[inline(always)]
    fn parse_fast(mut self) -> Result<ScalarExpr, XPathError> {
        self.skip_whitespace();
        if self.is_eof() {
            return Err(self.error("empty XPath predicate"));
        }
        let expression = self.parse_or_fast()?;
        self.skip_whitespace();
        if !self.is_eof() {
            return Err(self.error("unexpected XPath predicate token"));
        }
        Ok(expression)
    }

    #[inline(always)]
    fn parse_or_fast(&mut self) -> Result<ScalarExpr, XPathError> {
        let mut expression = self.parse_and_fast()?;
        loop {
            self.skip_whitespace();
            if !self.consume_keyword("or") {
                break;
            }
            expression = ScalarExpr::Or(Box::new(expression), Box::new(self.parse_and_fast()?));
        }
        Ok(expression)
    }

    #[inline(always)]
    fn parse_and_fast(&mut self) -> Result<ScalarExpr, XPathError> {
        let mut expression = self.parse_comparison_fast()?;
        loop {
            self.skip_whitespace();
            if !self.consume_keyword("and") {
                break;
            }
            expression = ScalarExpr::And(
                Box::new(expression),
                Box::new(self.parse_comparison_fast()?),
            );
        }
        Ok(expression)
    }

    #[inline(always)]
    fn parse_comparison_fast(&mut self) -> Result<ScalarExpr, XPathError> {
        let left = self.parse_additive_fast()?;
        self.skip_whitespace();
        let comparison = [
            ("!=", Comparison::NotEqual),
            ("<=", Comparison::LessOrEqual),
            (">=", Comparison::GreaterOrEqual),
            ("=", Comparison::Equal),
            ("<", Comparison::Less),
            (">", Comparison::Greater),
        ]
        .into_iter()
        .find_map(|(literal, comparison)| self.consume_literal(literal).then_some(comparison));
        if let Some(comparison) = comparison {
            let right = self.parse_additive_fast()?;
            Ok(ScalarExpr::Compare(
                comparison,
                Box::new(left),
                Box::new(right),
            ))
        } else {
            Ok(left)
        }
    }

    #[inline(always)]
    fn parse_additive_fast(&mut self) -> Result<ScalarExpr, XPathError> {
        let mut expression = self.parse_multiplicative_fast()?;
        loop {
            self.skip_whitespace();
            let operation = if self.consume_byte(b'+') {
                Some(Arithmetic::Add)
            } else if self.consume_byte(b'-') {
                Some(Arithmetic::Subtract)
            } else {
                None
            };
            let Some(operation) = operation else {
                break;
            };
            expression = ScalarExpr::Arithmetic(
                operation,
                Box::new(expression),
                Box::new(self.parse_multiplicative_fast()?),
            );
        }
        Ok(expression)
    }

    #[inline(always)]
    fn parse_multiplicative_fast(&mut self) -> Result<ScalarExpr, XPathError> {
        let mut expression = self.parse_unary_fast()?;
        loop {
            self.skip_whitespace();
            let operation = if self.consume_byte(b'*') {
                Some(Arithmetic::Multiply)
            } else if self.consume_keyword("div") {
                Some(Arithmetic::Divide)
            } else if self.consume_keyword("mod") {
                Some(Arithmetic::Modulo)
            } else {
                None
            };
            let Some(operation) = operation else {
                break;
            };
            expression = ScalarExpr::Arithmetic(
                operation,
                Box::new(expression),
                Box::new(self.parse_unary_fast()?),
            );
        }
        Ok(expression)
    }

    #[inline(always)]
    fn parse_unary_fast(&mut self) -> Result<ScalarExpr, XPathError> {
        self.skip_whitespace();
        if self.consume_byte(b'-') {
            return Ok(ScalarExpr::Negate(Box::new(self.parse_unary_fast()?)));
        }
        self.parse_primary_fast()
    }

    #[inline(always)]
    fn parse_primary_fast(&mut self) -> Result<ScalarExpr, XPathError> {
        self.skip_whitespace();
        if self.consume_byte(b'(') {
            let expression = self.parse_or_fast()?;
            self.skip_whitespace();
            self.expect_byte(b')', "expected ')' in XPath predicate")?;
            return Ok(expression);
        }
        if matches!(self.peek(), Some(b'\'' | b'"')) {
            return self.parse_string().map(ScalarExpr::String);
        }
        if self.starts_number() {
            return self.parse_number().map(ScalarExpr::Number);
        }
        if self.peek() == Some(b'$') {
            return self.parse_variable_fast();
        }
        if let Some(function) = self.parse_function_fast()? {
            return Ok(function);
        }
        self.parse_path().map(ScalarExpr::Path)
    }

    #[inline(always)]
    fn parse_function_fast(&mut self) -> Result<Option<ScalarExpr>, XPathError> {
        let saved = self.index;
        let start = self.index;
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            self.index += 1;
        }
        if self.index == start {
            return Ok(None);
        }
        let Some(function) = CoreFunction::parse(&self.input[start..self.index]) else {
            self.index = saved;
            return Ok(None);
        };
        self.skip_whitespace();
        if !self.consume_byte(b'(') {
            self.index = saved;
            return Ok(None);
        }

        let mut arguments = Vec::new();
        self.skip_whitespace();
        if !self.consume_byte(b')') {
            loop {
                arguments.push(self.parse_or_fast()?);
                self.skip_whitespace();
                if self.consume_byte(b')') {
                    break;
                }
                self.expect_byte(b',', "expected ',' between XPath function arguments")?;
            }
        }
        if !function.accepts(arguments.len()) {
            return Err(XPathError {
                message: "wrong number of XPath function arguments",
                byte: start,
            });
        }
        Ok(Some(ScalarExpr::Function {
            function,
            arguments,
            byte: start,
        }))
    }

    #[inline(always)]
    fn parse_variable_fast(&mut self) -> Result<ScalarExpr, XPathError> {
        let byte = self.index;
        self.index += 1;
        let start = self.index;
        let Some(first) = self.input[self.index..].chars().next() else {
            return Err(self.error("expected XPath variable name"));
        };
        if !crate::syntax::is_name_start_char(first) {
            return Err(self.error("expected XPath variable name"));
        }
        self.index += first.len_utf8();
        while let Some(character) = self.input[self.index..].chars().next() {
            if !crate::syntax::is_name_char(character) {
                break;
            }
            self.index += character.len_utf8();
        }
        let name = self.input[start..self.index].to_owned();
        crate::XmlQualifiedName::parse(&name).map_err(|_| XPathError {
            message: "invalid XPath variable name",
            byte,
        })?;
        Ok(ScalarExpr::Variable { name, byte })
    }

    #[cold]
    #[inline(never)]
    fn parse_with_depth_limit<const CHECK_DEPTH: bool>(mut self) -> Result<ScalarExpr, XPathError> {
        self.skip_whitespace();
        if self.is_eof() {
            return Err(self.error("empty XPath predicate"));
        }
        let expression = self.parse_or::<CHECK_DEPTH>()?;
        self.skip_whitespace();
        if !self.is_eof() {
            return Err(self.error("unexpected XPath predicate token"));
        }
        Ok(expression.expression)
    }

    fn parse_or<const CHECK_DEPTH: bool>(&mut self) -> Result<ParsedScalar, XPathError> {
        let mut expression = self.parse_and::<CHECK_DEPTH>()?;
        loop {
            self.skip_whitespace();
            let byte = self.index;
            if !self.consume_keyword("or") {
                break;
            }
            let right = self.parse_and::<CHECK_DEPTH>()?;
            expression =
                ParsedScalar::binary::<CHECK_DEPTH>(expression, right, byte, ScalarExpr::Or)?;
        }
        Ok(expression)
    }

    fn parse_and<const CHECK_DEPTH: bool>(&mut self) -> Result<ParsedScalar, XPathError> {
        let mut expression = self.parse_comparison::<CHECK_DEPTH>()?;
        loop {
            self.skip_whitespace();
            let byte = self.index;
            if !self.consume_keyword("and") {
                break;
            }
            let right = self.parse_comparison::<CHECK_DEPTH>()?;
            expression =
                ParsedScalar::binary::<CHECK_DEPTH>(expression, right, byte, ScalarExpr::And)?;
        }
        Ok(expression)
    }

    fn parse_comparison<const CHECK_DEPTH: bool>(&mut self) -> Result<ParsedScalar, XPathError> {
        let left = self.parse_additive::<CHECK_DEPTH>()?;
        self.skip_whitespace();
        let byte = self.index;
        let comparison = [
            ("!=", Comparison::NotEqual),
            ("<=", Comparison::LessOrEqual),
            (">=", Comparison::GreaterOrEqual),
            ("=", Comparison::Equal),
            ("<", Comparison::Less),
            (">", Comparison::Greater),
        ]
        .into_iter()
        .find_map(|(literal, comparison)| self.consume_literal(literal).then_some(comparison));
        if let Some(comparison) = comparison {
            let right = self.parse_additive::<CHECK_DEPTH>()?;
            ParsedScalar::binary::<CHECK_DEPTH>(left, right, byte, |left, right| {
                ScalarExpr::Compare(comparison, left, right)
            })
        } else {
            Ok(left)
        }
    }

    fn parse_additive<const CHECK_DEPTH: bool>(&mut self) -> Result<ParsedScalar, XPathError> {
        let mut expression = self.parse_multiplicative::<CHECK_DEPTH>()?;
        loop {
            self.skip_whitespace();
            let byte = self.index;
            let operation = if self.consume_byte(b'+') {
                Some(Arithmetic::Add)
            } else if self.consume_byte(b'-') {
                Some(Arithmetic::Subtract)
            } else {
                None
            };
            let Some(operation) = operation else {
                break;
            };
            let right = self.parse_multiplicative::<CHECK_DEPTH>()?;
            expression =
                ParsedScalar::binary::<CHECK_DEPTH>(expression, right, byte, |left, right| {
                    ScalarExpr::Arithmetic(operation, left, right)
                })?;
        }
        Ok(expression)
    }

    fn parse_multiplicative<const CHECK_DEPTH: bool>(
        &mut self,
    ) -> Result<ParsedScalar, XPathError> {
        let mut expression = self.parse_unary::<CHECK_DEPTH>()?;
        loop {
            self.skip_whitespace();
            let byte = self.index;
            let operation = if self.consume_byte(b'*') {
                Some(Arithmetic::Multiply)
            } else if self.consume_keyword("div") {
                Some(Arithmetic::Divide)
            } else if self.consume_keyword("mod") {
                Some(Arithmetic::Modulo)
            } else {
                None
            };
            let Some(operation) = operation else {
                break;
            };
            let right = self.parse_unary::<CHECK_DEPTH>()?;
            expression =
                ParsedScalar::binary::<CHECK_DEPTH>(expression, right, byte, |left, right| {
                    ScalarExpr::Arithmetic(operation, left, right)
                })?;
        }
        Ok(expression)
    }

    fn parse_unary<const CHECK_DEPTH: bool>(&mut self) -> Result<ParsedScalar, XPathError> {
        let mut operators = 0usize;
        let mut first = None;
        loop {
            self.skip_whitespace();
            let byte = self.index;
            if !self.consume_byte(b'-') {
                break;
            }
            first.get_or_insert(byte);
            operators += 1;
            if CHECK_DEPTH && operators >= MAX_XPATH_EXPRESSION_DEPTH {
                return Err(xpath_expression_depth_error(byte));
            }
        }
        let mut expression = self.parse_primary::<CHECK_DEPTH>()?;
        for _ in 0..operators {
            expression =
                ParsedScalar::unary::<CHECK_DEPTH>(expression, first.unwrap(), ScalarExpr::Negate)?;
        }
        Ok(expression)
    }

    fn parse_primary<const CHECK_DEPTH: bool>(&mut self) -> Result<ParsedScalar, XPathError> {
        self.skip_whitespace();
        if self.consume_byte(b'(') {
            let expression = self.parse_or::<CHECK_DEPTH>()?;
            self.skip_whitespace();
            self.expect_byte(b')', "expected ')' in XPath predicate")?;
            return Ok(expression);
        }
        if matches!(self.peek(), Some(b'\'' | b'"')) {
            return self
                .parse_string()
                .map(|value| ParsedScalar::leaf(ScalarExpr::String(value)));
        }
        if self.starts_number() {
            return self
                .parse_number()
                .map(|value| ParsedScalar::leaf(ScalarExpr::Number(value)));
        }
        if self.peek() == Some(b'$') {
            return self.parse_variable();
        }
        if let Some(function) = self.parse_function::<CHECK_DEPTH>()? {
            return Ok(function);
        }
        self.parse_path()
            .map(|path| ParsedScalar::leaf(ScalarExpr::Path(path)))
    }

    fn parse_function<const CHECK_DEPTH: bool>(
        &mut self,
    ) -> Result<Option<ParsedScalar>, XPathError> {
        let saved = self.index;
        let start = self.index;
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            self.index += 1;
        }
        if self.index == start {
            return Ok(None);
        }
        let Some(function) = CoreFunction::parse(&self.input[start..self.index]) else {
            self.index = saved;
            return Ok(None);
        };
        self.skip_whitespace();
        if !self.consume_byte(b'(') {
            self.index = saved;
            return Ok(None);
        }

        let mut arguments = Vec::new();
        self.skip_whitespace();
        if !self.consume_byte(b')') {
            loop {
                arguments.push(self.parse_or::<CHECK_DEPTH>()?);
                self.skip_whitespace();
                if self.consume_byte(b')') {
                    break;
                }
                self.expect_byte(b',', "expected ',' between XPath function arguments")?;
            }
        }
        if !function.accepts(arguments.len()) {
            return Err(XPathError {
                message: "wrong number of XPath function arguments",
                byte: start,
            });
        }
        ParsedScalar::function::<CHECK_DEPTH>(function, arguments, start).map(Some)
    }

    fn parse_variable(&mut self) -> Result<ParsedScalar, XPathError> {
        let byte = self.index;
        self.index += 1;
        let start = self.index;
        let Some(first) = self.input[self.index..].chars().next() else {
            return Err(self.error("expected XPath variable name"));
        };
        if !crate::syntax::is_name_start_char(first) {
            return Err(self.error("expected XPath variable name"));
        }
        self.index += first.len_utf8();
        while let Some(character) = self.input[self.index..].chars().next() {
            if !crate::syntax::is_name_char(character) {
                break;
            }
            self.index += character.len_utf8();
        }
        let name = self.input[start..self.index].to_owned();
        crate::XmlQualifiedName::parse(&name).map_err(|_| XPathError {
            message: "invalid XPath variable name",
            byte,
        })?;
        Ok(ParsedScalar::leaf(ScalarExpr::Variable { name, byte }))
    }

    fn parse_path(&mut self) -> Result<LocationPath, XPathError> {
        let start = self.index;
        let mut parentheses = 0usize;
        let mut brackets = 0usize;
        let mut quote = None;
        while let Some(byte) = self.peek() {
            if let Some(active) = quote {
                self.index += 1;
                if byte == active {
                    quote = None;
                }
                continue;
            }
            match byte {
                b'\'' | b'"' if brackets > 0 => {
                    quote = Some(byte);
                    self.index += 1;
                }
                b'[' => {
                    brackets += 1;
                    self.index += 1;
                }
                b']' if brackets > 0 => {
                    brackets -= 1;
                    self.index += 1;
                }
                b'(' => {
                    parentheses += 1;
                    self.index += 1;
                }
                b')' if parentheses > 0 => {
                    parentheses -= 1;
                    self.index += 1;
                }
                b')' | b',' | b'=' | b'!' | b'<' | b'>' if parentheses == 0 && brackets == 0 => {
                    break;
                }
                b'+' if parentheses == 0 && brackets == 0 => break,
                b'*' if parentheses == 0
                    && brackets == 0
                    && self.index > start
                    && !matches!(
                        self.input.as_bytes().get(self.index.wrapping_sub(1)),
                        Some(b'/' | b':' | b'@')
                    ) =>
                {
                    break;
                }
                b'-' if parentheses == 0
                    && brackets == 0
                    && self
                        .input
                        .as_bytes()
                        .get(self.index + 1)
                        .is_some_and(u8::is_ascii_digit) =>
                {
                    break;
                }
                byte if byte.is_ascii_whitespace() && parentheses == 0 && brackets == 0 => break,
                _ => self.index += 1,
            }
        }
        if start == self.index {
            return Err(self.error("expected XPath predicate value"));
        }
        let mut path = PathParser::new(&self.input[start..self.index])
            .parse()
            .map_err(|mut error| {
                error.byte += start;
                error
            })?;
        path.shift_variable_bytes(start);
        Ok(path)
    }

    fn parse_string(&mut self) -> Result<String, XPathError> {
        let quote = self.peek().expect("caller checked quote");
        self.index += 1;
        let start = self.index;
        while self.peek().is_some_and(|byte| byte != quote) {
            self.index += 1;
        }
        if !self.consume_byte(quote) {
            return Err(self.error("unterminated XPath string literal"));
        }
        Ok(self.input[start..self.index - 1].to_owned())
    }

    fn parse_number(&mut self) -> Result<f64, XPathError> {
        let start = self.index;
        if self.peek() == Some(b'-') {
            self.index += 1;
        }
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.index += 1;
        }
        if self.peek() == Some(b'.') {
            self.index += 1;
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.index += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.index += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.index += 1;
            }
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.index += 1;
            }
        }
        self.input[start..self.index]
            .parse()
            .map_err(|_| XPathError {
                message: "invalid XPath number",
                byte: start,
            })
    }

    fn starts_number(&self) -> bool {
        match (
            self.peek(),
            self.input.as_bytes().get(self.index + 1).copied(),
        ) {
            (Some(byte), _) if byte.is_ascii_digit() => true,
            (Some(b'.'), Some(next)) => next.is_ascii_digit(),
            _ => false,
        }
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        if !self.input[self.index..].starts_with(keyword) {
            return false;
        }
        let end = self.index + keyword.len();
        if self.input[end..]
            .chars()
            .next()
            .is_some_and(crate::syntax::is_name_char)
        {
            return false;
        }
        self.index = end;
        true
    }

    fn expect_byte(&mut self, byte: u8, message: &'static str) -> Result<(), XPathError> {
        if self.consume_byte(byte) {
            Ok(())
        } else {
            Err(self.error(message))
        }
    }

    fn consume_literal(&mut self, literal: &str) -> bool {
        if self.input[self.index..].starts_with(literal) {
            self.index += literal.len();
            true
        } else {
            false
        }
    }

    fn consume_byte(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.index += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.as_bytes().get(self.index).copied()
    }

    fn is_eof(&self) -> bool {
        self.index == self.input.len()
    }

    fn error(&self, message: &'static str) -> XPathError {
        XPathError {
            message,
            byte: self.index,
        }
    }
}
