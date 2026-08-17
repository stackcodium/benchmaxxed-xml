use std::{error::Error, fmt};

/// The standard result type returned by XML parsing and validation operations.
pub type XmlResult<T> = Result<T, XmlError>;

/// A strict XML parse error and its zero-based source byte offset.
///
/// String entry points report an offset in the supplied UTF-8 string. Byte-oriented entry points
/// report an offset in the original encoded byte slice, including a byte-order mark when present.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XmlError {
    /// The category and associated error details.
    pub kind: XmlErrorKind,
    /// Zero-based byte offset at which parsing failed.
    pub byte: usize,
}

impl XmlError {
    pub(crate) fn new(kind: XmlErrorKind, byte: usize) -> Self {
        Self { kind, byte }
    }
}

/// Categories of strict XML parse and validation failures.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XmlErrorKind {
    /// Element nesting exceeds the parser's fixed safety limit.
    DepthLimitExceeded,
    /// Recursive internal-entity expansion exceeds its configured depth limit.
    EntityExpansionDepthLimitExceeded,
    /// Expanded internal-entity output exceeds its configured byte limit.
    EntityExpansionSizeLimitExceeded,
    /// The named entity requires expansion while expansion is disabled.
    EntityExpansionDisabled(String),
    /// Entity replacement would create markup that cannot borrow original-source ranges.
    EntityReplacementMarkupWithSourceOffsets,
    /// The named external entity cannot be loaded under the no-I/O external-entity policy.
    ExternalEntityReference(String),
    /// An element contains the named attribute more than once.
    DuplicateAttribute(String),
    /// The parser expected the described token or construct.
    Expected(&'static str),
    /// An attribute value violates XML lexical rules.
    InvalidAttributeValue,
    /// The input contains a scalar value forbidden by the selected XML version.
    InvalidCharacter,
    /// A numeric character reference is malformed or resolves to a forbidden character.
    InvalidCharacterReference,
    /// A comment contains forbidden syntax such as `--`.
    InvalidComment,
    /// Document-level content is ordered or nested incorrectly.
    InvalidDocumentStructure,
    /// An element, attribute, entity, or processing-instruction name is invalid.
    InvalidName,
    /// A processing instruction uses a forbidden target.
    InvalidProcessingInstructionTarget,
    /// The XML declaration is malformed or internally inconsistent.
    InvalidXmlDeclaration,
    /// Indicates `MismatchedEndTag`.
    MismatchedEndTag {
        /// The open-element name required at this position.
        expected: String,
        /// The end-tag name found in the input.
        found: String,
    },
    /// No document element was present.
    MissingRootElement,
    /// Non-miscellaneous content follows the document element.
    TrailingContent,
    /// A reference names an entity that was not declared.
    UndeclaredEntity(String),
    /// The declaration requests an unsupported encoding.
    UnsupportedEncoding(String),
    /// Input ended before the current construct was complete.
    UnexpectedEof,
    /// The next token is not legal in the current context.
    UnexpectedToken,
}

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.kind, self.byte)
    }
}

impl fmt::Display for XmlErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DepthLimitExceeded => f.write_str("element depth limit exceeded"),
            Self::EntityExpansionDepthLimitExceeded => {
                f.write_str("entity expansion depth limit exceeded")
            }
            Self::EntityExpansionSizeLimitExceeded => {
                f.write_str("entity expansion size limit exceeded")
            }
            Self::EntityExpansionDisabled(name) => {
                write!(f, "entity expansion is disabled for {name:?}")
            }
            Self::EntityReplacementMarkupWithSourceOffsets => f.write_str(
                "markup-producing entity expansion is incompatible with source-backed views",
            ),
            Self::ExternalEntityReference(name) => {
                write!(f, "external entity {name:?} cannot be loaded implicitly")
            }
            Self::DuplicateAttribute(name) => write!(f, "duplicate attribute {name:?}"),
            Self::Expected(expected) => write!(f, "expected {expected}"),
            Self::InvalidAttributeValue => f.write_str("invalid attribute value"),
            Self::InvalidCharacter => f.write_str("invalid XML character"),
            Self::InvalidCharacterReference => f.write_str("invalid character reference"),
            Self::InvalidComment => f.write_str("invalid comment"),
            Self::InvalidDocumentStructure => f.write_str("invalid document structure"),
            Self::InvalidName => f.write_str("invalid XML name"),
            Self::InvalidProcessingInstructionTarget => {
                f.write_str("invalid processing instruction target")
            }
            Self::InvalidXmlDeclaration => f.write_str("invalid XML declaration"),
            Self::MismatchedEndTag { expected, found } => {
                write!(
                    f,
                    "mismatched end tag: expected {expected:?}, found {found:?}"
                )
            }
            Self::MissingRootElement => f.write_str("missing root element"),
            Self::TrailingContent => f.write_str("trailing content after root element"),
            Self::UndeclaredEntity(name) => write!(f, "undeclared entity {name:?}"),
            Self::UnsupportedEncoding(encoding) => write!(f, "unsupported encoding {encoding:?}"),
            Self::UnexpectedEof => f.write_str("unexpected end of input"),
            Self::UnexpectedToken => f.write_str("unexpected token"),
        }
    }
}

impl Error for XmlError {}
