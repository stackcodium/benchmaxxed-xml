#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![doc = include_str!("../README.md")]

mod cursor;
mod dom;
mod dom_api;
mod dom_facade;
mod dtd;
mod encoding;
mod error;
mod fragment;
mod io;
mod mutation;
mod namespace;
mod parser;
mod serialize;
mod source;
mod syntax;
mod value;
mod xpath_engine;

pub use cursor::XmlPath;
pub use dom::{
    RawXmlAttribute, RawXmlNode, XmlAttribute, XmlAttributeView, XmlCompactDocument,
    XmlCompactNode, XmlDoctype, XmlDocumentView, XmlElement, XmlMemoryRetention, XmlNode,
    XmlNodeKind, XmlProcessingInstruction, XmlRawSource, XmlTreeStats, XmlViewNode, XmlViewNodeId,
};
pub use dom_api::{XmlDescendants, XmlNodeRef, XmlWalk, XmlWalkEntry};
pub use dom_facade::{
    XmlDom, XmlDomElementBuilder, XmlDomError, XmlDomNode, XmlDomNodeId, XmlDomNodeSet,
    XmlDomOutputError, XmlDomScanAttribute, XmlDomScanAttributes, XmlDomScanNode, XmlDomSend,
    XmlDomWalk, XmlDomXPathNode, XmlSourceCoordinates, XmlSourcePosition,
};
pub use encoding::{DecodedXml, XmlInputEncoding, decode_xml_bytes};
pub use error::{XmlError, XmlErrorKind, XmlResult};
pub use fragment::{
    XmlFragment, parse_fragment, parse_fragment_tolerant, parse_fragment_tolerant_with_config,
    parse_fragment_with_config,
};
pub use io::XmlLoadError;
pub use mutation::XmlMutationError;
pub use namespace::{
    XML_NAMESPACE_URI, XMLNS_NAMESPACE_URI, XmlExpandedName, XmlNamespace, XmlNamespaceError,
    XmlQualifiedName,
};
pub use parser::{
    ParserConfig, XmlAttributeWhitespacePolicy, XmlEntityExpansionPolicy, XmlExternalEntityPolicy,
    XmlParseOutcome, XmlParser, XmlTextWhitespacePolicy, XmlVersion, count_document,
    count_document_bytes, count_document_bytes_with_config, count_document_with_config,
    parse_compact_document, parse_compact_document_bytes, parse_compact_document_bytes_tolerant,
    parse_compact_document_bytes_tolerant_with_config, parse_compact_document_bytes_with_config,
    parse_compact_document_tolerant, parse_compact_document_tolerant_with_config,
    parse_compact_document_with_config, parse_document_view, parse_document_view_with_config,
    parse_document_view_with_config_and_source_offsets, parse_document_view_with_source_offsets,
    validate_document, validate_document_bytes, validate_document_bytes_with_config,
    validate_document_with_config,
};
pub use serialize::{
    XmlDeclarationMode, XmlEscapeMode, XmlOutputEncoding, XmlQuoteStyle, XmlSerializeOptions,
    XmlWriteError,
};
pub use source::{
    XmlDocumentViewWithSourceOffsets, XmlSourceAttribute, XmlSourceNode, XmlSourceNodeId,
    XmlSourceOffsets, XmlSourceSpan,
};
pub use value::{ToXmlValue, XmlValueError};
pub use xpath_engine::{
    XPathContext, XPathError, XPathExpression, XPathNamespaces, XPathNode, XPathVariable,
    XPathVariables,
};
