use std::{error::Error, fmt};

/// A rejected DOM mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XmlMutationError {
    /// A constructed element, attribute, doctype, or processing-instruction name is not XML-safe.
    InvalidName(String),
    /// Character data contains a code point that XML 1.0 cannot represent.
    InvalidCharacter,
    /// A comment contains `--` or ends in `-`.
    InvalidComment,
    /// A processing instruction has a reserved target or contains `?>` in its data.
    InvalidProcessingInstruction,
    /// An element contains the same attribute name more than once.
    DuplicateAttribute(String),
    /// A document type declaration is internally inconsistent or malformed.
    InvalidDoctype,
    /// XML declaration pseudo-attributes are malformed.
    InvalidDeclaration,
    /// The requested logical path no longer resolves in the document.
    InvalidPath,
    /// An operation attempted to remove or position a sibling around the document element.
    RootHasNoSiblings,
    /// A child insertion or move targeted a non-element node.
    DestinationNotElement,
    /// An insertion or replacement index exceeds the child collection length.
    IndexOutOfBounds {
        /// The requested index.
        index: usize,
        /// The collection length at the time of the operation.
        len: usize,
    },
    /// A move attempted to make a node its own ancestor.
    MoveIntoDescendant,
}

impl fmt::Display for XmlMutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(name) => write!(formatter, "invalid XML name {name:?}"),
            Self::InvalidCharacter => formatter.write_str("invalid XML character"),
            Self::InvalidComment => formatter.write_str("invalid XML comment"),
            Self::InvalidProcessingInstruction => {
                formatter.write_str("invalid XML processing instruction")
            }
            Self::DuplicateAttribute(name) => {
                write!(formatter, "duplicate XML attribute {name:?}")
            }
            Self::InvalidDoctype => formatter.write_str("invalid XML doctype"),
            Self::InvalidDeclaration => formatter.write_str("invalid XML declaration"),
            Self::InvalidPath => formatter.write_str("XML mutation path does not exist"),
            Self::RootHasNoSiblings => {
                formatter.write_str("the document element cannot be removed or have siblings")
            }
            Self::DestinationNotElement => {
                formatter.write_str("XML mutation destination is not an element")
            }
            Self::IndexOutOfBounds { index, len } => {
                write!(formatter, "XML child index {index} exceeds length {len}")
            }
            Self::MoveIntoDescendant => {
                formatter.write_str("an XML node cannot be moved into its own subtree")
            }
        }
    }
}

impl Error for XmlMutationError {}

#[inline]
pub(crate) fn validate_name(name: &str) -> Result<(), XmlMutationError> {
    crate::syntax::is_valid_name(name)
        .then_some(())
        .ok_or_else(|| XmlMutationError::InvalidName(name.to_owned()))
}

#[inline]
pub(crate) fn validate_characters(value: &str) -> Result<(), XmlMutationError> {
    let valid = value
        .as_bytes()
        .iter()
        .all(|byte| *byte >= b' ' || matches!(*byte, b'\t' | b'\n' | b'\r'))
        && !value.contains('\u{fffe}')
        && !value.contains('\u{ffff}');
    valid
        .then_some(())
        .ok_or(XmlMutationError::InvalidCharacter)
}

/// Validates a constructed subtree before it enters a mutable document.
pub(crate) fn validate_node(node: &crate::XmlNode) -> Result<(), XmlMutationError> {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        match node {
            crate::XmlNode::Element(element) => {
                validate_name(&element.name)?;
                for (index, attribute) in element.attributes.iter().enumerate() {
                    validate_name(&attribute.name)?;
                    validate_characters(&attribute.value)?;
                    if element.attributes[..index]
                        .iter()
                        .any(|previous| previous.name == attribute.name)
                    {
                        return Err(XmlMutationError::DuplicateAttribute(attribute.name.clone()));
                    }
                }
                pending.extend(element.children.iter().rev());
            }
            crate::XmlNode::Text(value) | crate::XmlNode::Cdata(value) => {
                validate_characters(value)?;
            }
            crate::XmlNode::Comment(value) => {
                validate_characters(value)?;
                if value.contains("--") || value.ends_with('-') {
                    return Err(XmlMutationError::InvalidComment);
                }
            }
            crate::XmlNode::ProcessingInstruction(pi) => {
                validate_name(&pi.target)?;
                validate_characters(&pi.data)?;
                if crate::syntax::is_xml_target(&pi.target) || pi.data.contains("?>") {
                    return Err(XmlMutationError::InvalidProcessingInstruction);
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_node_value(
    kind: crate::XmlNodeKind,
    value: &str,
) -> Result<(), XmlMutationError> {
    validate_characters(value)?;
    match kind {
        crate::XmlNodeKind::Comment if value.contains("--") || value.ends_with('-') => {
            Err(XmlMutationError::InvalidComment)
        }
        crate::XmlNodeKind::ProcessingInstruction if value.contains("?>") => {
            Err(XmlMutationError::InvalidProcessingInstruction)
        }
        _ => Ok(()),
    }
}

pub(crate) fn validate_doctype(doctype: &crate::XmlDoctype) -> Result<(), XmlMutationError> {
    validate_name(&doctype.name)?;
    if doctype.public_id.is_some() && doctype.system_id.is_none() {
        return Err(XmlMutationError::InvalidDoctype);
    }
    if doctype
        .public_id
        .as_deref()
        .is_some_and(|value| !value.chars().all(crate::syntax::is_pubid_char))
    {
        return Err(XmlMutationError::InvalidDoctype);
    }
    for value in [doctype.public_id.as_deref(), doctype.system_id.as_deref()]
        .into_iter()
        .flatten()
    {
        validate_characters(value)?;
        if value.contains('"') && value.contains('\'') {
            return Err(XmlMutationError::InvalidDoctype);
        }
    }
    if let Some(subset) = &doctype.internal_subset {
        crate::dtd::validate_internal_subset(subset, 0, false)
            .map_err(|_| XmlMutationError::InvalidDoctype)?;
    }
    Ok(())
}
