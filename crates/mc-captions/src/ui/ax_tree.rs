//! The bounded tree walk of the macOS backend, over an abstract element source
//! so depth limits, node budgets, deadlines and vanished nodes are tested
//! without a Mac.

use super::backend::{Budget, UiError};
use super::mirror::Mirror;
use super::node::UiNode;

/// One element read from the platform: its node (without children) and the
/// elements below it.
pub struct Read<E> {
    pub node: UiNode,
    pub children: Vec<E>,
}

pub trait AxSource {
    type Elem: Clone;

    /// `Stale` when the element vanished (it is skipped, not fatal).
    fn read(&mut self, element: &Self::Elem) -> Result<Read<Self::Elem>, UiError>;
}

/// A clipped subtree with the live elements mirrored by path.
pub struct Walk<E> {
    pub tree: UiNode,
    pub mirror: Mirror<E>,
    /// The deadline or the node budget cut the walk short.
    pub truncated: bool,
}

struct State {
    count: usize,
    truncated: bool,
}

/// The subtree at `root` clipped to the budget's depth, node count and deadline; like the Windows cache read, a
/// cut walk returns what it has (flagged) because a partial tree still holds the captions seen so far.
pub fn read_tree<S: AxSource>(
    source: &mut S,
    root: &S::Elem,
    budget: &Budget,
) -> Result<Walk<S::Elem>, UiError> {
    if budget.expired() {
        return Err(UiError::Budget);
    }
    let mut state = State {
        count: 0,
        truncated: false,
    };
    let (tree, mirror) = walk(source, root, 0, budget, &mut state)?;
    Ok(Walk {
        tree,
        mirror,
        truncated: state.truncated,
    })
}

fn walk<S: AxSource>(
    source: &mut S,
    element: &S::Elem,
    depth: usize,
    budget: &Budget,
    state: &mut State,
) -> Result<(UiNode, Mirror<S::Elem>), UiError> {
    state.count += 1;
    let Read { mut node, children } = source.read(element)?;
    let mut mirror = Mirror::leaf(element.clone());
    if depth >= budget.max_depth {
        return Ok((node, mirror));
    }
    for child in &children {
        if state.count >= budget.max_nodes || budget.expired() {
            state.truncated = true;
            break;
        }
        match walk(source, child, depth + 1, budget, state) {
            Ok((child_node, child_mirror)) => {
                node.children.push(child_node);
                mirror.children.push(child_mirror);
            }
            // A node that vanished mid-walk (live captions churn) only shortens the list.
            Err(UiError::Stale) => {}
            Err(e) => return Err(e),
        }
    }
    Ok((node, mirror))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::ui::node::Role;

    /// Elements are indexes into a table of (name, children, readable).
    struct Table(Vec<(&'static str, Vec<usize>, bool)>);

    impl AxSource for Table {
        type Elem = usize;

        fn read(&mut self, element: &usize) -> Result<Read<usize>, UiError> {
            let (name, children, ok) = &self.0[*element];
            if !ok {
                return Err(UiError::Stale);
            }
            Ok(Read {
                node: UiNode::new(Role::Group, name),
                children: children.clone(),
            })
        }
    }

    fn sample() -> Table {
        Table(vec![
            ("root", vec![1, 2], true),
            ("a", vec![3], true),
            ("b", vec![], true),
            ("a1", vec![], true),
        ])
    }

    #[test]
    fn reads_the_whole_tree_and_mirrors_it() {
        let Walk {
            tree,
            mirror,
            truncated,
        } = read_tree(&mut sample(), &0, &Budget::new(10, 100)).unwrap();
        assert!(!truncated);
        assert_eq!(tree.count(), 4);
        assert_eq!(tree.at(&[0, 0]).unwrap().name, "a1");
        assert_eq!(mirror.at(&[0, 0]).unwrap().elem, 3);
    }

    #[test]
    fn depth_clips_the_walk() {
        let walk = read_tree(&mut sample(), &0, &Budget::new(1, 100)).unwrap();
        assert_eq!(walk.tree.count(), 3);
        assert!(!walk.truncated);
    }

    #[test]
    fn node_budget_truncates_instead_of_failing() {
        let walk = read_tree(&mut sample(), &0, &Budget::new(10, 2)).unwrap();
        assert_eq!(walk.tree.count(), 2);
        assert_eq!(walk.mirror.children.len(), 1);
        assert!(walk.truncated);
    }

    #[test]
    fn expired_deadline_before_the_root_is_a_budget_error() {
        let mut budget = Budget::new(10, 100);
        budget.deadline = Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(
            read_tree(&mut sample(), &0, &budget).err(),
            Some(UiError::Budget)
        );
    }

    /// The third read stalls 200 ms, past a 150 ms deadline (timer slack cannot reorder this).
    struct Slow(Table, usize);

    impl AxSource for Slow {
        type Elem = usize;

        fn read(&mut self, element: &usize) -> Result<Read<usize>, UiError> {
            self.1 += 1;
            if self.1 == 3 {
                std::thread::sleep(Duration::from_millis(200));
            }
            self.0.read(element)
        }
    }

    #[test]
    fn a_deadline_mid_walk_returns_the_partial_tree() {
        let mut budget = Budget::new(10, 100);
        budget.deadline = Some(Instant::now() + Duration::from_millis(150));
        let walk = read_tree(&mut Slow(sample(), 0), &0, &budget).unwrap();
        assert!(walk.truncated);
        assert!(walk.tree.count() < 4);
        assert!(walk.tree.count() >= 2);
    }

    #[test]
    fn vanished_children_are_skipped_and_a_vanished_root_fails() {
        let mut table = sample();
        table.0[1].2 = false;
        let walk = read_tree(&mut table, &0, &Budget::new(10, 100)).unwrap();
        assert_eq!(walk.tree.count(), 2);
        assert_eq!(walk.mirror.children[0].elem, 2);
        assert_eq!(
            read_tree(&mut table, &1, &Budget::new(10, 100)).err(),
            Some(UiError::Stale)
        );
    }
}
