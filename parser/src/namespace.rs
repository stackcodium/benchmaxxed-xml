use std::{error::Error, fmt};

use crate::{XmlAttribute, XmlElement};

/// The XML NAMESPACE URI.
pub const XML_NAMESPACE_URI: &str = "http://www.w3.org/XML/1998/namespace";
/// The XMLNS NAMESPACE URI.
pub const XMLNS_NAMESPACE_URI: &str = "http://www.w3.org/2000/xmlns/";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// A public `XmlQualifiedName` value in the XML data model.
pub struct XmlQualifiedName<'a> {
    /// The qualified.
    pub qualified: &'a str,
    /// The prefix.
    pub prefix: Option<&'a str>,
    /// The local.
    pub local: &'a str,
}

impl<'a> XmlQualifiedName<'a> {
    /// Parses the value.
    pub fn parse(name: &'a str) -> Result<Self, XmlNamespaceError> {
        let (prefix, local) = match name.split_once(':') {
            Some((prefix, local)) if !local.contains(':') => (Some(prefix), local),
            Some(_) => return Err(XmlNamespaceError::InvalidQualifiedName(name.to_owned())),
            None => (None, name),
        };
        if local.is_empty()
            || prefix.is_some_and(str::is_empty)
            || !valid_ncname(local)
            || prefix.is_some_and(|prefix| !valid_ncname(prefix))
        {
            return Err(XmlNamespaceError::InvalidQualifiedName(name.to_owned()));
        }
        Ok(Self {
            qualified: name,
            prefix,
            local,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// A public `XmlNamespace` value in the XML data model.
pub struct XmlNamespace<'a> {
    /// The prefix.
    pub prefix: Option<&'a str>,
    /// The uri.
    pub uri: &'a str,
}

/// An owned namespace-expanded element name returned by the mutable DOM facade.
///
/// `qualified` preserves the lexical spelling used by the document. Namespace identity is the
/// pair `(namespace_uri, local)`; `prefix` is retained for callers that also need the source-level
/// name. An unprefixed element uses the in-scope default namespace when one exists.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct XmlExpandedName {
    /// The lexical qualified name, such as `feed:entry`.
    pub qualified: String,
    /// The lexical prefix, without `:`, when present.
    pub prefix: Option<String>,
    /// The local part of the name.
    pub local: String,
    /// The resolved in-scope namespace URI, when the name is bound.
    pub namespace_uri: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// The supported `XmlNamespaceError` alternatives.
pub enum XmlNamespaceError {
    /// Indicates `InvalidQualifiedName`.
    InvalidQualifiedName(String),
}

impl fmt::Display for XmlNamespaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidQualifiedName(name) => {
                write!(formatter, "invalid namespace-qualified name {name:?}")
            }
        }
    }
}

impl Error for XmlNamespaceError {}

impl XmlElement {
    /// Returns qualified name.
    pub fn qualified_name(&self) -> Result<XmlQualifiedName<'_>, XmlNamespaceError> {
        XmlQualifiedName::parse(&self.name)
    }

    /// Returns prefix.
    pub fn prefix(&self) -> Result<Option<&str>, XmlNamespaceError> {
        Ok(self.qualified_name()?.prefix)
    }

    /// Returns local name.
    pub fn local_name(&self) -> Result<&str, XmlNamespaceError> {
        Ok(self.qualified_name()?.local)
    }

    /// Returns namespace declarations.
    pub fn namespace_declarations(&self) -> impl Iterator<Item = XmlNamespace<'_>> {
        self.attributes.iter().filter_map(|attribute| {
            if attribute.name == "xmlns" {
                Some(XmlNamespace {
                    prefix: None,
                    uri: &attribute.value,
                })
            } else {
                attribute
                    .name
                    .strip_prefix("xmlns:")
                    .map(|prefix| XmlNamespace {
                        prefix: Some(prefix),
                        uri: &attribute.value,
                    })
            }
        })
    }
}

impl XmlAttribute {
    /// Returns qualified name.
    pub fn qualified_name(&self) -> Result<XmlQualifiedName<'_>, XmlNamespaceError> {
        XmlQualifiedName::parse(&self.name)
    }

    /// Returns prefix.
    pub fn prefix(&self) -> Result<Option<&str>, XmlNamespaceError> {
        Ok(self.qualified_name()?.prefix)
    }

    /// Returns local name.
    pub fn local_name(&self) -> Result<&str, XmlNamespaceError> {
        Ok(self.qualified_name()?.local)
    }

    /// Returns whether this value is namespace declaration.
    pub fn is_namespace_declaration(&self) -> bool {
        self.name == "xmlns" || self.name.starts_with("xmlns:")
    }
}

fn valid_ncname(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character != ':' && crate::syntax::is_name_start_char(character))
        && characters.all(|character| character != ':' && crate::syntax::is_name_char(character))
}

#[cfg(test)]
mod tests {
    use crate::XmlQualifiedName;

    #[test]
    fn splits_valid_qualified_names() {
        let name = XmlQualifiedName::parse("feed:entry").unwrap();
        assert_eq!(name.prefix, Some("feed"));
        assert_eq!(name.local, "entry");
        assert!(XmlQualifiedName::parse("a:b:c").is_err());
        assert!(XmlQualifiedName::parse(":entry").is_err());
    }
}
