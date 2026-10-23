//! A snapshot's live elements, addressable by child-index path.
//!
//! The Windows backend builds one alongside every whole-window snapshot so a
//! later poll can go straight to the remembered container (one cross-process
//! call) instead of walking down to it.

#[derive(Debug, Clone, PartialEq)]
pub struct Mirror<E> {
    pub elem: E,
    pub children: Vec<Mirror<E>>,
}

impl<E> Mirror<E> {
    pub fn leaf(elem: E) -> Self {
        Self {
            elem,
            children: Vec::new(),
        }
    }

    /// The node at `path` (empty: this one), if the path exists.
    pub fn at(&self, path: &[usize]) -> Option<&Self> {
        path.iter()
            .try_fold(self, |node, index| node.children.get(*index))
    }
}
