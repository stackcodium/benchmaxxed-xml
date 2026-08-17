use std::{error::Error, fmt, io, path::PathBuf};

use crate::XmlError;

#[derive(Debug)]
/// The supported `XmlLoadError` alternatives.
pub enum XmlLoadError {
    /// Indicates `Io`.
    Io {
        /// The path.
        path: Option<PathBuf>,
        /// The io.
        source: io::Error,
    },
    /// Indicates `Parse`.
    Parse(XmlError),
}

impl fmt::Display for XmlLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                path: Some(path),
                source,
            } => write!(
                formatter,
                "failed to read XML file {}: {source}",
                path.display()
            ),
            Self::Io { path: None, source } => {
                write!(formatter, "failed to read XML input: {source}")
            }
            Self::Parse(error) => write!(formatter, "failed to parse XML input: {error}"),
        }
    }
}

impl Error for XmlLoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse(error) => Some(error),
        }
    }
}

impl From<XmlError> for XmlLoadError {
    fn from(error: XmlError) -> Self {
        Self::Parse(error)
    }
}
