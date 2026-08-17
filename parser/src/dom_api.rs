use std::str::FromStr;

use crate::{
    XmlAttribute, XmlDoctype, XmlElement, XmlMemoryRetention, XmlMutationError, XmlNode,
    XmlProcessingInstruction,
};

impl XmlDoctype {
    /// Creates a doctype with a validated document-element name and no external identifier or
    /// internal subset.
    pub fn new(name: impl Into<String>) -> Result<Self, XmlMutationError> {
        let name = name.into();
        crate::mutation::validate_name(&name)?;
        Ok(Self {
            name,
            public_id: None,
            system_id: None,
            internal_subset: None,
        })
    }

    /// Returns the declared document-element name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the public external identifier, when present.
    pub fn public_id(&self) -> Option<&str> {
        self.public_id.as_deref()
    }

    /// Returns the system external identifier, when present.
    pub fn system_id(&self) -> Option<&str> {
        self.system_id.as_deref()
    }

    /// Returns the internal subset without its surrounding brackets.
    pub fn internal_subset(&self) -> Option<&str> {
        self.internal_subset.as_deref()
    }

    /// Sets the public external identifier.
    pub fn set_public_id(&mut self, value: Option<String>) -> Result<(), XmlMutationError> {
        if value
            .as_deref()
            .is_some_and(|value| !value.chars().all(crate::syntax::is_pubid_char))
        {
            return Err(XmlMutationError::InvalidCharacter);
        }
        self.public_id = value;
        Ok(())
    }

    /// Sets the system external identifier.
    pub fn set_system_id(&mut self, value: Option<String>) -> Result<(), XmlMutationError> {
        if let Some(value) = &value {
            crate::mutation::validate_characters(value)?;
        }
        self.system_id = value;
        Ok(())
    }

    /// Sets the internal subset without its surrounding brackets.
    pub fn set_internal_subset(&mut self, value: Option<String>) -> Result<(), XmlMutationError> {
        if let Some(value) = &value {
            crate::mutation::validate_characters(value)?;
        }
        self.internal_subset = value;
        Ok(())
    }
}

impl XmlElement {
    /// Returns the element name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Renames the element after validating the new XML name.
    pub fn set_name(&mut self, name: impl Into<String>) -> Result<(), XmlMutationError> {
        let name = name.into();
        crate::mutation::validate_name(&name)?;
        self.name = name;
        Ok(())
    }

    /// Returns attributes in document order.
    pub fn attributes(&self) -> &[XmlAttribute] {
        &self.attributes
    }

    /// Returns child nodes in document order.
    pub fn children(&self) -> &[XmlNode] {
        &self.children
    }

    /// Creates a validated value.
    pub fn new(name: impl Into<String>) -> Result<Self, XmlMutationError> {
        Self::with_capacity(name, 0, 0)
    }

    pub(crate) fn new_unchecked(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Creates an element with capacity reserved for predictable bulk construction.
    pub fn with_capacity(
        name: impl Into<String>,
        attribute_capacity: usize,
        child_capacity: usize,
    ) -> Result<Self, XmlMutationError> {
        let name = name.into();
        crate::mutation::validate_name(&name)?;
        Ok(Self {
            name,
            attributes: Vec::with_capacity(attribute_capacity),
            children: Vec::with_capacity(child_capacity),
        })
    }

    /// Creates an element containing one PCDATA child.
    pub fn with_text(
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, XmlMutationError> {
        let name = name.into();
        let value = value.into();
        crate::mutation::validate_name(&name)?;
        crate::mutation::validate_characters(&value)?;
        Ok(Self {
            name,
            attributes: Vec::new(),
            children: vec![XmlNode::Text(value)],
        })
    }

    /// Returns attribute.
    pub fn attribute(&self, name: &str) -> Option<&XmlAttribute> {
        self.attributes
            .iter()
            .find(|attribute| attribute.name == name)
    }

    /// Returns mutable access to attribute when present.
    pub fn attribute_mut(&mut self, name: &str) -> Option<&mut XmlAttribute> {
        self.attributes
            .iter_mut()
            .find(|attribute| attribute.name == name)
    }

    /// Appends an attribute when the caller already knows its name is not present.
    pub fn append_attribute(
        &mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<&mut XmlAttribute, XmlMutationError> {
        self.attributes.push(XmlAttribute::new(name, value)?);
        Ok(self.attributes.last_mut().unwrap())
    }

    /// Updates the first matching attribute or appends a new attribute.
    pub fn set_attribute(
        &mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<&mut XmlAttribute, XmlMutationError> {
        let name = name.into();
        let value = value.into();
        crate::mutation::validate_name(&name)?;
        crate::mutation::validate_characters(&value)?;
        Ok(self.set_attribute_unchecked(name, value))
    }

    pub(crate) fn set_attribute_unchecked(
        &mut self,
        name: String,
        value: String,
    ) -> &mut XmlAttribute {
        if let Some(index) = self
            .attributes
            .iter()
            .position(|attribute| attribute.name == name)
        {
            self.attributes[index].value = value;
            return &mut self.attributes[index];
        }
        self.attributes
            .push(XmlAttribute::new_unchecked(name, value));
        self.attributes.last_mut().unwrap()
    }

    /// Prepends attribute.
    pub fn prepend_attribute(
        &mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<&mut XmlAttribute, XmlMutationError> {
        self.attributes.insert(0, XmlAttribute::new(name, value)?);
        Ok(&mut self.attributes[0])
    }

    /// Inserts attribute.
    pub fn insert_attribute(
        &mut self,
        index: usize,
        attribute: XmlAttribute,
    ) -> Result<&mut XmlAttribute, XmlMutationError> {
        if index > self.attributes.len() {
            return Err(XmlMutationError::IndexOutOfBounds {
                index,
                len: self.attributes.len(),
            });
        }
        self.attributes.insert(index, attribute);
        Ok(&mut self.attributes[index])
    }

    /// Removes attribute.
    pub fn remove_attribute(&mut self, name: &str) -> Option<XmlAttribute> {
        let index = self
            .attributes
            .iter()
            .position(|attribute| attribute.name == name)?;
        Some(self.attributes.remove(index))
    }

    /// Clears attributes.
    pub fn clear_attributes(&mut self) {
        self.attributes.clear();
    }

    /// Returns first child.
    pub fn first_child(&self) -> Option<&XmlNode> {
        self.children.first()
    }

    /// Returns mutable access to first child when present.
    pub fn first_child_mut(&mut self) -> Option<&mut XmlNode> {
        self.children.first_mut()
    }

    /// Returns last child.
    pub fn last_child(&self) -> Option<&XmlNode> {
        self.children.last()
    }

    /// Returns mutable access to last child when present.
    pub fn last_child_mut(&mut self) -> Option<&mut XmlNode> {
        self.children.last_mut()
    }

    /// Returns child.
    pub fn child(&self, name: &str) -> Option<&XmlElement> {
        self.children.iter().find_map(|node| match node {
            XmlNode::Element(element) if element.name == name => Some(element),
            _ => None,
        })
    }

    /// Returns mutable access to child when present.
    pub fn child_mut(&mut self, name: &str) -> Option<&mut XmlElement> {
        self.children.iter_mut().find_map(|node| match node {
            XmlNode::Element(element) if element.name == name => Some(element),
            _ => None,
        })
    }

    /// Returns children named.
    pub fn children_named<'a>(
        &'a self,
        name: &'a str,
    ) -> impl DoubleEndedIterator<Item = &'a XmlElement> + 'a {
        self.children.iter().filter_map(move |node| match node {
            XmlNode::Element(element) if element.name == name => Some(element),
            _ => None,
        })
    }

    /// Returns element children.
    pub fn element_children(&self) -> impl DoubleEndedIterator<Item = &XmlElement> {
        self.children.iter().filter_map(XmlNode::as_element)
    }

    /// Appends child.
    pub fn append_child(&mut self, node: XmlNode) -> Result<&mut XmlNode, XmlMutationError> {
        crate::mutation::validate_node(&node)?;
        self.children.push(node);
        Ok(self.children.last_mut().unwrap())
    }

    /// Prepends child.
    pub fn prepend_child(&mut self, node: XmlNode) -> Result<&mut XmlNode, XmlMutationError> {
        crate::mutation::validate_node(&node)?;
        self.children.insert(0, node);
        Ok(&mut self.children[0])
    }

    /// Inserts child.
    pub fn insert_child(
        &mut self,
        index: usize,
        node: XmlNode,
    ) -> Result<&mut XmlNode, XmlMutationError> {
        crate::mutation::validate_node(&node)?;
        if index > self.children.len() {
            return Err(XmlMutationError::IndexOutOfBounds {
                index,
                len: self.children.len(),
            });
        }
        self.children.insert(index, node);
        Ok(&mut self.children[index])
    }

    /// Replaces child.
    pub fn replace_child(
        &mut self,
        index: usize,
        node: XmlNode,
    ) -> Result<XmlNode, XmlMutationError> {
        crate::mutation::validate_node(&node)?;
        let len = self.children.len();
        let Some(slot) = self.children.get_mut(index) else {
            return Err(XmlMutationError::IndexOutOfBounds { index, len });
        };
        Ok(std::mem::replace(slot, node))
    }

    /// Removes a child without shrinking the reusable child-vector capacity.
    pub fn remove_child_at(&mut self, index: usize) -> Option<XmlNode> {
        self.remove_child_at_with_retention(index, XmlMemoryRetention::RetainCapacity)
    }

    /// Removes child at with retention.
    pub fn remove_child_at_with_retention(
        &mut self,
        index: usize,
        retention: XmlMemoryRetention,
    ) -> Option<XmlNode> {
        if index >= self.children.len() {
            return None;
        }
        let removed = self.children.remove(index);
        if retention == XmlMemoryRetention::ReleaseSpareCapacity {
            self.children.shrink_to_fit();
        }
        Some(removed)
    }

    /// Removes child.
    pub fn remove_child(&mut self, name: &str) -> Option<XmlNode> {
        let index = self
            .children
            .iter()
            .position(|node| matches!(node, XmlNode::Element(element) if element.name == name))?;
        self.remove_child_at(index)
    }

    /// Clears children.
    pub fn clear_children(&mut self) {
        self.children.clear();
    }

    /// Appends element.
    pub fn append_element(
        &mut self,
        name: impl Into<String>,
    ) -> Result<&mut XmlElement, XmlMutationError> {
        let name = name.into();
        crate::mutation::validate_name(&name)?;
        Ok(self.append_element_unchecked(name))
    }

    pub(crate) fn append_element_unchecked(&mut self, name: String) -> &mut XmlElement {
        self.children
            .push(XmlNode::Element(Self::new_unchecked(name)));
        self.children.last_mut().unwrap().as_element_mut().unwrap()
    }

    /// Prepends element.
    pub fn prepend_element(
        &mut self,
        name: impl Into<String>,
    ) -> Result<&mut XmlElement, XmlMutationError> {
        self.children.insert(0, XmlNode::Element(Self::new(name)?));
        Ok(self.children[0].as_element_mut().unwrap())
    }

    /// Returns or creates child.
    pub fn ensure_child(
        &mut self,
        name: impl Into<String>,
    ) -> Result<&mut XmlElement, XmlMutationError> {
        let name = name.into();
        crate::mutation::validate_name(&name)?;
        if let Some(index) = self
            .children
            .iter()
            .position(|node| matches!(node, XmlNode::Element(element) if element.name == name))
        {
            return Ok(self.children[index].as_element_mut().unwrap());
        }
        Ok(self.append_element_unchecked(name))
    }

    /// Returns the first immediate PCDATA or CDATA value.
    pub fn text(&self) -> Option<&str> {
        self.children.iter().find_map(|node| match node {
            XmlNode::Text(value) | XmlNode::Cdata(value) => Some(value.as_str()),
            _ => None,
        })
    }

    /// Updates the first immediate text node, or prepends a new PCDATA node.
    pub fn set_text(&mut self, value: impl Into<String>) -> Result<&mut String, XmlMutationError> {
        let value = value.into();
        crate::mutation::validate_characters(&value)?;
        Ok(self.set_text_unchecked(value))
    }

    pub(crate) fn set_text_unchecked(&mut self, value: String) -> &mut String {
        let index = self
            .children
            .iter()
            .position(|node| matches!(node, XmlNode::Text(_) | XmlNode::Cdata(_)));
        if let Some(index) = index {
            match &mut self.children[index] {
                XmlNode::Text(text) | XmlNode::Cdata(text) => {
                    *text = value;
                    return text;
                }
                _ => unreachable!(),
            }
        }
        self.children.insert(0, XmlNode::Text(value));
        match &mut self.children[0] {
            XmlNode::Text(text) => text,
            _ => unreachable!(),
        }
    }

    /// Concatenates all descendant PCDATA and CDATA in document order.
    pub fn text_content(&self) -> String {
        let mut output = String::new();
        for entry in self.walk() {
            if let XmlNodeRef::Text(value) | XmlNodeRef::Cdata(value) = entry.node {
                output.push_str(value);
            }
        }
        output
    }

    /// Parses the first immediate text value without conflating absence with invalid text.
    pub fn parse_text<T: FromStr>(&self) -> Result<Option<T>, T::Err> {
        self.text().map(str::parse).transpose()
    }

    /// Returns descendants.
    pub fn descendants(&self) -> XmlDescendants<'_> {
        let mut stack: Vec<_> = self.element_children().collect();
        stack.reverse();
        XmlDescendants { stack }
    }

    /// Walks the element and all of its content in pre-order.
    pub fn walk(&self) -> XmlWalk<'_> {
        XmlWalk {
            stack: vec![(XmlNodeRef::Element(self), 0)],
        }
    }
}

impl XmlAttribute {
    /// Returns the attribute name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the decoded attribute value.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Replaces the decoded value after validating its XML characters.
    pub fn set_value(&mut self, value: impl Into<String>) -> Result<(), XmlMutationError> {
        let value = value.into();
        crate::mutation::validate_characters(&value)?;
        self.value = value;
        Ok(())
    }

    /// Creates a validated value.
    pub fn new(
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, XmlMutationError> {
        let name = name.into();
        let value = value.into();
        crate::mutation::validate_name(&name)?;
        crate::mutation::validate_characters(&value)?;
        Ok(Self { name, value })
    }

    pub(crate) fn new_unchecked(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }

    /// Returns this value as str when it has that kind.
    pub fn as_str(&self) -> &str {
        &self.value
    }

    /// Parses the value.
    pub fn parse<T: FromStr>(&self) -> Result<T, T::Err> {
        self.value.parse()
    }

    /// Uses pugixml-compatible first-character boolean conversion.
    pub fn as_bool(&self, default: bool) -> bool {
        self.value.as_bytes().first().map_or(default, |byte| {
            matches!(byte, b'1' | b't' | b'T' | b'y' | b'Y')
        })
    }
}

impl XmlNode {
    /// Returns element.
    pub fn element(name: impl Into<String>) -> Result<Self, XmlMutationError> {
        Ok(Self::Element(XmlElement::new(name)?))
    }

    pub(crate) fn element_unchecked(name: impl Into<String>) -> Self {
        Self::Element(XmlElement::new_unchecked(name))
    }

    /// Returns this value as element when it has that kind.
    pub fn as_element(&self) -> Option<&XmlElement> {
        match self {
            Self::Element(element) => Some(element),
            _ => None,
        }
    }

    /// Returns this value as element mut when it has that kind.
    pub fn as_element_mut(&mut self) -> Option<&mut XmlElement> {
        match self {
            Self::Element(element) => Some(element),
            _ => None,
        }
    }

    /// Returns value.
    pub fn value(&self) -> Option<&str> {
        match self {
            Self::Element(_) => None,
            Self::Text(value) | Self::Comment(value) | Self::Cdata(value) => Some(value),
            Self::ProcessingInstruction(pi) => Some(&pi.data),
        }
    }
}

impl XmlProcessingInstruction {
    /// Creates a non-declaration processing instruction.
    pub fn new(
        target: impl Into<String>,
        data: impl Into<String>,
    ) -> Result<Self, XmlMutationError> {
        let target = target.into();
        let data = data.into();
        crate::mutation::validate_name(&target)?;
        crate::mutation::validate_characters(&data)?;
        if crate::syntax::is_xml_target(&target) || data.contains("?>") {
            return Err(XmlMutationError::InvalidProcessingInstruction);
        }
        Ok(Self { target, data })
    }

    /// Returns the processing-instruction target.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Returns processing-instruction data without `?>`.
    pub fn data(&self) -> &str {
        &self.data
    }

    /// Replaces processing-instruction data after validation.
    pub fn set_data(&mut self, data: impl Into<String>) -> Result<(), XmlMutationError> {
        let data = data.into();
        crate::mutation::validate_characters(&data)?;
        if data.contains("?>") {
            return Err(XmlMutationError::InvalidProcessingInstruction);
        }
        self.data = data;
        Ok(())
    }
}

/// A public `XmlDescendants` value in the XML data model.
pub struct XmlDescendants<'a> {
    stack: Vec<&'a XmlElement>,
}

impl<'a> Iterator for XmlDescendants<'a> {
    type Item = &'a XmlElement;

    fn next(&mut self) -> Option<Self::Item> {
        let element = self.stack.pop()?;
        let children: Vec<_> = element.element_children().collect();
        self.stack.extend(children.into_iter().rev());
        Some(element)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
/// The supported `XmlNodeRef` alternatives.
pub enum XmlNodeRef<'a> {
    /// Indicates `Element`.
    Element(&'a XmlElement),
    /// Indicates `Text`.
    Text(&'a str),
    /// Indicates `Comment`.
    Comment(&'a str),
    /// Indicates `Cdata`.
    Cdata(&'a str),
    /// Indicates `ProcessingInstruction`.
    ProcessingInstruction(&'a XmlProcessingInstruction),
}

impl<'a> From<&'a XmlNode> for XmlNodeRef<'a> {
    fn from(node: &'a XmlNode) -> Self {
        match node {
            XmlNode::Element(element) => Self::Element(element),
            XmlNode::Text(value) => Self::Text(value),
            XmlNode::Comment(value) => Self::Comment(value),
            XmlNode::Cdata(value) => Self::Cdata(value),
            XmlNode::ProcessingInstruction(pi) => Self::ProcessingInstruction(pi),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
/// A public `XmlWalkEntry` value in the XML data model.
pub struct XmlWalkEntry<'a> {
    /// The node.
    pub node: XmlNodeRef<'a>,
    /// The depth.
    pub depth: usize,
}

/// A public `XmlWalk` value in the XML data model.
pub struct XmlWalk<'a> {
    stack: Vec<(XmlNodeRef<'a>, usize)>,
}

impl<'a> Iterator for XmlWalk<'a> {
    type Item = XmlWalkEntry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let (node, depth) = self.stack.pop()?;
        if let XmlNodeRef::Element(element) = node {
            self.stack.extend(
                element
                    .children
                    .iter()
                    .rev()
                    .map(|child| (XmlNodeRef::from(child), depth + 1)),
            );
        }
        Some(XmlWalkEntry { node, depth })
    }
}
