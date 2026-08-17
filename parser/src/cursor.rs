use std::hash::{Hash, Hasher};

/// A stable, document-relative sequence of child indexes.
///
/// The empty path identifies the document element. A path remains usable while mutation does not
/// remove its target or change a child index on the route to it.
#[derive(Clone, Debug, Default)]
pub struct XmlPath {
    indexes: Vec<usize>,
}

impl PartialEq for XmlPath {
    fn eq(&self, other: &Self) -> bool {
        self.indexes == other.indexes
    }
}

impl Eq for XmlPath {}

impl Hash for XmlPath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.indexes.hash(state);
    }
}

impl XmlPath {
    /// Returns the path of the document element.
    pub fn root() -> Self {
        Self::default()
    }

    /// Returns a path extended with one child index.
    pub fn child(&self, index: usize) -> Self {
        let mut indexes = self.indexes.clone();
        indexes.push(index);
        Self { indexes }
    }

    /// Returns this path's parent, or `None` for the document element.
    pub fn parent(&self) -> Option<Self> {
        let mut indexes = self.indexes.clone();
        indexes.pop()?;
        Some(Self { indexes })
    }

    /// Returns the child indexes from the document element to the target.
    pub fn indexes(&self) -> &[usize] {
        &self.indexes
    }

    /// Returns whether this path identifies the document element.
    pub fn is_root(&self) -> bool {
        self.indexes.is_empty()
    }

    pub(crate) fn from_indexes(indexes: Vec<usize>) -> Self {
        Self { indexes }
    }

    pub(crate) fn indexes_mut(&mut self) -> &mut Vec<usize> {
        &mut self.indexes
    }
}
