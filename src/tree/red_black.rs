//! An ordered map backed by a left-leaning red-black tree.
//!
//! # Invariants
//!
//! Every node is coloured red or black, and empty links count as black. Outside of an
//! in-progress `insert` or `remove`, the tree always satisfies:
//!
//! 1. **Black root.** The root is black.
//! 2. **No red-red.** A red node never has a red child.
//! 3. **Equal black height.** Every path from a node down to an empty link passes through the
//!    same number of black nodes.
//! 4. **Left-leaning.** A red node is always a left child.
//! 5. **Search order.** Every key in a node's left subtree is smaller than the node's key, and
//!    every key in its right subtree is larger.
//!
//! Invariants 2 and 3 are what keep the tree balanced. A path holds at least as many black nodes
//! as the black height `b`, and since red nodes cannot be stacked, at most as many red nodes as
//! black ones. So no path is more than twice as long as any other, the tree holds at least
//! `2^b - 1` nodes, and its height is at most `2·log2(n + 1)`. Every operation walks a single
//! path, so it takes `O(log n)` steps.
//!
//! Invariant 4 is what makes this tree "left-leaning". A red node can be read as glued to its
//! parent, the pair forming one node with two keys and three children in a 2-3 tree. Forcing the
//! red node to the left gives each such pair a single shape, which halves the number of cases
//! insertion and removal have to handle.
//!
//! # Why a left-leaning tree
//!
//! The classic red-black tree (as in Cormen et al.) repairs the tree after an insertion or
//! removal by walking back up from the changed node, so every node needs a link to its parent.
//! In safe Rust that means either `Rc<RefCell<_>>` or an arena of nodes addressed by index, where
//! every lookup can fail and has to be handled, since this crate denies panics. The left-leaning
//! variant ([Sedgewick, 2008](https://sedgewick.io/wp-content/themes/sedgewick/papers/2008LLRB.pdf))
//! repairs each node on the way back out of a recursive descent instead, so a node only needs to
//! own its two children, which a `Box` does. The trade-offs accepted are more rotations than the
//! classic tree (every node on the search path is repaired, not only the ones that need it) and
//! recursion as deep as the tree, which the height bound above keeps below 128 levels.

use std::{
    borrow::Borrow,
    cmp::Ordering,
    fmt,
    iter::FusedIterator,
    mem,
    ops::{Bound, RangeBounds},
};

/// An ordered map from keys to values, kept balanced as a left-leaning red-black tree.
///
/// Lookups, insertions and removals take `O(log n)` steps, and iteration visits the entries in
/// ascending key order. The API mirrors the parts of [`BTreeMap`](std::collections::BTreeMap)
/// that a balanced binary search tree supports.
///
/// # Examples
///
/// ```
/// use db_data_structures::tree::red_black::RedBlackTree;
///
/// let mut tree = RedBlackTree::new();
/// tree.insert(3, "three");
/// tree.insert(1, "one");
/// assert_eq!(tree.insert(3, "THREE"), Some("three"));
///
/// assert_eq!(tree.get(&3), Some(&"THREE"));
/// assert_eq!(tree.min(), Some((&1, &"one")));
/// assert_eq!(tree.remove(&1), Some("one"));
/// assert_eq!(tree.iter().collect::<Vec<_>>(), [(&3, &"THREE")]);
/// ```
#[derive(Clone)]
pub struct RedBlackTree<K, V> {
    root: Link<K, V>,
    len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Color {
    Red,
    Black,
}

impl Color {
    fn flipped(self) -> Self {
        match self {
            Self::Red => Self::Black,
            Self::Black => Self::Red,
        }
    }
}

type Link<K, V> = Option<Box<Node<K, V>>>;

#[derive(Clone)]
struct Node<K, V> {
    key: K,
    value: V,
    color: Color,
    left: Link<K, V>,
    right: Link<K, V>,
}

impl<K, V> Node<K, V> {
    /// New nodes are red so that adding one never changes a black height (invariant 3); the
    /// red-red or right-leaning links it may create are repaired by `fix_up`.
    fn new_red(key: K, value: V) -> Box<Self> {
        Box::new(Self {
            key,
            value,
            color: Color::Red,
            left: None,
            right: None,
        })
    }
}

fn is_red<K, V>(link: &Link<K, V>) -> bool {
    link.as_ref().is_some_and(|node| node.color == Color::Red)
}

fn has_red_left_child<K, V>(link: &Link<K, V>) -> bool {
    link.as_ref().is_some_and(|node| is_red(&node.left))
}

impl<K, V> RedBlackTree<K, V> {
    pub const fn new() -> Self {
        Self { root: None, len: 0 }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the entry with the smallest key.
    pub fn min(&self) -> Option<(&K, &V)> {
        let mut node = self.root.as_deref()?;
        while let Some(left) = node.left.as_deref() {
            node = left;
        }
        Some((&node.key, &node.value))
    }

    /// Returns the entry with the largest key.
    pub fn max(&self) -> Option<(&K, &V)> {
        let mut node = self.root.as_deref()?;
        while let Some(right) = node.right.as_deref() {
            node = right;
        }
        Some((&node.key, &node.value))
    }

    /// Iterates over every entry in ascending key order.
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter {
            nodes: InOrder::from_first(self.root.as_deref()),
            remaining: self.len,
        }
    }
}

impl<K: Ord, V> RedBlackTree<K, V> {
    /// Inserts `value` under `key`, and returns the value it replaced, if any.
    ///
    /// Like [`BTreeMap::insert`](std::collections::BTreeMap::insert), replacing a value keeps
    /// the key that was already stored.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        let (mut root, previous) = insert_into(self.root.take(), key, value);
        // `fix_up` may leave a red link pointing at the root (splitting a 4-node at the top of a
        // 2-3 tree); recolouring it black restores invariant 1 and grows every black height by
        // one, so invariant 3 still holds.
        root.color = Color::Black;
        self.root = Some(root);
        if previous.is_none() {
            self.len = self.len.saturating_add(1);
        }
        previous
    }

    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.find(key).map(|node| &node.value)
    }

    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut current = self.root.as_deref_mut();
        while let Some(node) = current {
            current = match key.cmp(node.key.borrow()) {
                Ordering::Less => node.left.as_deref_mut(),
                Ordering::Greater => node.right.as_deref_mut(),
                Ordering::Equal => return Some(&mut node.value),
            };
        }
        None
    }

    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.find(key).is_some()
    }

    /// Removes `key` from the tree, and returns the value it held, if any.
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        // `remove_from` reshapes the tree on its way down before it knows whether the key is
        // there. Reshaping towards a missing key would leave the colour flips of
        // `move_red_left`/`move_red_right` applied below nodes with no children to absorb them,
        // breaking invariant 3, so a missing key must never reach it.
        if !self.contains_key(key) {
            return None;
        }
        // Sedgewick's version first colours the root red when both its children are black. That
        // step is left out because it cannot change the result: no step of the descent reads
        // the colour of the node it stands on, only its children's, so the root's colour only
        // ever decides the colour of whichever node ends up on top, which is recoloured black
        // here anyway.
        let (mut root, removed) = remove_from(self.root.take(), key);
        if let Some(root) = root.as_mut() {
            root.color = Color::Black;
        }
        self.root = root;
        if removed.is_some() {
            self.len = self.len.saturating_sub(1);
        }
        removed
    }

    /// Iterates in ascending key order over the entries whose keys fall in `range`.
    ///
    /// Unlike [`BTreeMap::range`](std::collections::BTreeMap::range), a range whose start is
    /// after its end is empty rather than a panic.
    ///
    /// # Examples
    ///
    /// ```
    /// use db_data_structures::tree::red_black::RedBlackTree;
    ///
    /// let mut tree = RedBlackTree::new();
    /// for key in 0..10 {
    ///     tree.insert(key, key * 10);
    /// }
    ///
    /// let keys: Vec<_> = tree.range(3..6).map(|(key, _)| *key).collect();
    /// assert_eq!(keys, [3, 4, 5]);
    /// ```
    pub fn range<Q, R>(&self, range: R) -> Range<'_, K, V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
        R: RangeBounds<Q>,
    {
        let last = self.last_within(range.end_bound());
        let nodes = match last {
            Some(_) => InOrder::from_start(self.root.as_deref(), range.start_bound()),
            None => InOrder::empty(),
        };
        Range { nodes, last }
    }

    fn find<Q>(&self, key: &Q) -> Option<&Node<K, V>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut current = self.root.as_deref();
        while let Some(node) = current {
            current = match key.cmp(node.key.borrow()) {
                Ordering::Less => node.left.as_deref(),
                Ordering::Greater => node.right.as_deref(),
                Ordering::Equal => return Some(node),
            };
        }
        None
    }

    /// Returns the largest key that does not go past `end`.
    fn last_within<Q>(&self, end: Bound<&Q>) -> Option<&K>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut last = None;
        let mut current = self.root.as_deref();
        while let Some(node) = current {
            if is_before_end(node.key.borrow(), end) {
                last = Some(&node.key);
                current = node.right.as_deref();
            } else {
                current = node.left.as_deref();
            }
        }
        last
    }
}

impl<K, V> Default for RedBlackTree<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: fmt::Debug, V: fmt::Debug> fmt::Debug for RedBlackTree<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<'a, K, V> IntoIterator for &'a RedBlackTree<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Inserts into the subtree at `link` and returns its new root, which may be red: the caller
/// either repairs it with its own `fix_up` or, at the top, recolours it black.
fn insert_into<K: Ord, V>(link: Link<K, V>, key: K, value: V) -> (Box<Node<K, V>>, Option<V>) {
    let Some(mut node) = link else {
        return (Node::new_red(key, value), None);
    };
    let previous = match key.cmp(&node.key) {
        Ordering::Less => {
            let (left, previous) = insert_into(node.left.take(), key, value);
            node.left = Some(left);
            previous
        },
        Ordering::Greater => {
            let (right, previous) = insert_into(node.right.take(), key, value);
            node.right = Some(right);
            previous
        },
        Ordering::Equal => Some(mem::replace(&mut node.value, value)),
    };
    (fix_up(node), previous)
}

/// Removes `key` from the subtree at `link` and returns its new root.
///
/// Expects the key to be in the subtree (see `RedBlackTree::remove`). Before each step down,
/// the descent makes sure the child it moves to is red or has a red left child, borrowing a red
/// link from the child's sibling or merging the two, so the node finally unlinked is red (or is
/// the last node of the tree) and removing it leaves every black height unchanged
/// (invariant 3). `fix_up` then undoes, on the way back up, the right-leaning and red-red links
/// the descent created.
fn remove_from<K, V, Q>(link: Link<K, V>, key: &Q) -> (Link<K, V>, Option<V>)
where
    K: Borrow<Q>,
    Q: Ord + ?Sized,
{
    let Some(mut node) = link else {
        return (None, None);
    };
    let removed = if key < node.key.borrow() {
        if !is_red(&node.left) && !has_red_left_child(&node.left) {
            node = move_red_left(node);
        }
        let (left, removed) = remove_from(node.left.take(), key);
        node.left = left;
        removed
    } else {
        if is_red(&node.left) {
            node = rotate_right(node);
        }
        if key == node.key.borrow() && node.right.is_none() {
            // The node is a leaf: without a right child, invariant 3 rules out a black left
            // child, and a red one was just rotated away.
            let Node { left, value, .. } = *node;
            return (left, Some(value));
        }
        if !is_red(&node.right) && !has_red_left_child(&node.right) {
            node = move_red_right(node);
        }
        if key == node.key.borrow() {
            // An inner node cannot be unlinked without orphaning a subtree, so it takes over its
            // successor's entry and the successor, a leaf, is unlinked instead.
            let (right, successor) = remove_min(node.right.take());
            node.right = right;
            successor.map(|(key, value)| {
                node.key = key;
                mem::replace(&mut node.value, value)
            })
        } else {
            let (right, removed) = remove_from(node.right.take(), key);
            node.right = right;
            removed
        }
    };
    (Some(fix_up(node)), removed)
}

/// Removes the smallest entry from the subtree at `link`, under the same expectations and by
/// the same steps as `remove_from`.
fn remove_min<K, V>(link: Link<K, V>) -> (Link<K, V>, Option<(K, V)>) {
    let Some(mut node) = link else {
        return (None, None);
    };
    if node.left.is_none() {
        // A node without a left child is a leaf: invariant 4 rules out a red right child, and
        // invariant 3 a black one.
        let Node {
            key, value, right, ..
        } = *node;
        return (right, Some((key, value)));
    }
    if !is_red(&node.left) && !has_red_left_child(&node.left) {
        node = move_red_left(node);
    }
    let (left, min) = remove_min(node.left.take());
    node.left = left;
    (Some(fix_up(node)), min)
}

/// Restores invariants 2 and 4 at `node`, given that they hold in both of its subtrees.
///
/// Each step fixes one local shape, and each can create the shape the next one fixes:
///
/// 1. A red right child next to a black left one leans the wrong way; rotate it to the left.
/// 2. Two red links in a row down the left; rotate the upper one to the right, which leaves a
///    node with two red children.
/// 3. Two red children are a 4-node in 2-3 terms; flipping colours splits it and passes the red
///    link up to the parent, whose own `fix_up` deals with it.
///
/// None of the steps changes a black height, so invariant 3 is preserved throughout.
fn fix_up<K, V>(mut node: Box<Node<K, V>>) -> Box<Node<K, V>> {
    if is_red(&node.right) && !is_red(&node.left) {
        node = rotate_left(node);
    }
    if is_red(&node.left) && has_red_left_child(&node.left) {
        node = rotate_right(node);
    }
    if is_red(&node.left) && is_red(&node.right) {
        flip_colors(&mut node);
    }
    node
}

/// Turns a red right link into a red left link:
///
/// ```text
///     node              right
///    /    \    =>      /     \
///   a     right      node     c
///        /     \    /    \
///       b       c  a      b
/// ```
///
/// The new top takes `node`'s colour and `node` becomes red, so black heights are unchanged.
/// Only called when `node` has a right child; without one, `node` is returned as it was.
fn rotate_left<K, V>(mut node: Box<Node<K, V>>) -> Box<Node<K, V>> {
    let Some(mut right) = node.right.take() else {
        return node;
    };
    node.right = right.left.take();
    right.color = node.color;
    node.color = Color::Red;
    right.left = Some(node);
    right
}

/// The mirror image of [`rotate_left`].
fn rotate_right<K, V>(mut node: Box<Node<K, V>>) -> Box<Node<K, V>> {
    let Some(mut left) = node.left.take() else {
        return node;
    };
    node.left = left.right.take();
    left.color = node.color;
    node.color = Color::Red;
    left.right = Some(node);
    left
}

/// Flips the colour of `node` and both of its children. With a black node and two red children
/// this splits a 4-node and passes a red link up; the other way round it merges `node` with its
/// children into a 4-node, which is how removal borrows a red link from above.
fn flip_colors<K, V>(node: &mut Node<K, V>) {
    node.color = node.color.flipped();
    for child in [node.left.as_mut(), node.right.as_mut()]
        .into_iter()
        .flatten()
    {
        child.color = child.color.flipped();
    }
}

/// Makes `node.left` or one of its children red, given that `node` is red and `node.left` and
/// `node.left.left` are black. Merges `node` with both children, then, if the right child has a
/// red link to spare, moves it across to the left instead.
fn move_red_left<K, V>(mut node: Box<Node<K, V>>) -> Box<Node<K, V>> {
    flip_colors(&mut node);
    if has_red_left_child(&node.right) {
        node.right = node.right.take().map(rotate_right);
        node = rotate_left(node);
        flip_colors(&mut node);
    }
    node
}

/// The mirror image of [`move_red_left`], making `node.right` or one of its children red.
fn move_red_right<K, V>(mut node: Box<Node<K, V>>) -> Box<Node<K, V>> {
    flip_colors(&mut node);
    if has_red_left_child(&node.left) {
        node = rotate_right(node);
        flip_colors(&mut node);
    }
    node
}

fn is_at_or_after_start<Q: Ord + ?Sized>(key: &Q, start: Bound<&Q>) -> bool {
    match start {
        Bound::Included(start) => key >= start,
        Bound::Excluded(start) => key > start,
        Bound::Unbounded => true,
    }
}

fn is_before_end<Q: Ord + ?Sized>(key: &Q, end: Bound<&Q>) -> bool {
    match end {
        Bound::Included(end) => key <= end,
        Bound::Excluded(end) => key < end,
        Bound::Unbounded => true,
    }
}

/// Walks nodes in ascending key order without recursion.
///
/// The stack holds the nodes still to visit whose left subtrees have already been visited or
/// skipped, with the next node on top. Visiting a node pushes the left spine of its right
/// subtree, whose smallest key comes next.
struct InOrder<'a, K, V> {
    stack: Vec<&'a Node<K, V>>,
}

impl<'a, K, V> InOrder<'a, K, V> {
    const fn empty() -> Self {
        Self { stack: Vec::new() }
    }

    fn from_first(root: Option<&'a Node<K, V>>) -> Self {
        let mut nodes = Self::empty();
        nodes.push_left_spine(root);
        nodes
    }

    /// Starts at the first key at or after `start`. Nodes before `start` are skipped along with
    /// their left subtrees, which hold only smaller keys; nodes at or after it are pushed so
    /// they are visited once their left subtrees are.
    fn from_start<Q>(root: Option<&'a Node<K, V>>, start: Bound<&Q>) -> Self
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut nodes = Self::empty();
        let mut current = root;
        while let Some(node) = current {
            if is_at_or_after_start(node.key.borrow(), start) {
                nodes.stack.push(node);
                current = node.left.as_deref();
            } else {
                current = node.right.as_deref();
            }
        }
        nodes
    }

    fn push_left_spine(&mut self, mut link: Option<&'a Node<K, V>>) {
        while let Some(node) = link {
            self.stack.push(node);
            link = node.left.as_deref();
        }
    }

    fn next_node(&mut self) -> Option<&'a Node<K, V>> {
        let node = self.stack.pop()?;
        self.push_left_spine(node.right.as_deref());
        Some(node)
    }
}

/// An iterator over the entries of a [`RedBlackTree`] in ascending key order, created by
/// [`RedBlackTree::iter`].
pub struct Iter<'a, K, V> {
    nodes: InOrder<'a, K, V>,
    remaining: usize,
}

impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.nodes.next_node()?;
        self.remaining = self.remaining.saturating_sub(1);
        Some((&node.key, &node.value))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<K, V> ExactSizeIterator for Iter<'_, K, V> {}

impl<K, V> FusedIterator for Iter<'_, K, V> {}

/// An iterator over the entries of a [`RedBlackTree`] whose keys fall in a range, in ascending
/// key order, created by [`RedBlackTree::range`].
pub struct Range<'a, K, V> {
    nodes: InOrder<'a, K, V>,
    /// The largest key in the range, found up front so the iterator does not have to keep the
    /// caller's bound alive. `None` when no key is at or before the end of the range.
    last: Option<&'a K>,
}

impl<'a, K: Ord, V> Iterator for Range<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        let last = self.last?;
        let node = self.nodes.next_node()?;
        if node.key > *last {
            self.last = None;
            return None;
        }
        Some((&node.key, &node.value))
    }
}

impl<K: Ord, V> FusedIterator for Range<'_, K, V> {}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test arithmetic works on small known values, and an overflow panic would only fail the test"
)]
mod tests {
    use std::{collections::BTreeMap, fmt::Debug};

    use rstest::rstest;

    use super::*;

    /// Panics unless `tree` satisfies every invariant listed in the module documentation, and
    /// its stored length matches its node count.
    fn check_invariants<K: Ord + Debug, V>(tree: &RedBlackTree<K, V>) {
        assert!(!is_red(&tree.root), "invariant 1: the root is red");
        let (_, node_count) = check_subtree(tree.root.as_deref(), None, None);
        assert_eq!(node_count, tree.len(), "len does not match the node count");
    }

    /// Checks invariants 2 to 5 in the subtree at `node`, whose keys must all lie strictly
    /// between `lower` and `upper`. Returns its black height and node count.
    fn check_subtree<K: Ord + Debug, V>(
        node: Option<&Node<K, V>>,
        lower: Option<&K>,
        upper: Option<&K>,
    ) -> (usize, usize) {
        let Some(node) = node else {
            // Empty links count as black.
            return (1, 0);
        };
        if let Some(lower) = lower {
            assert!(
                node.key > *lower,
                "invariant 5: {:?} is left of {lower:?}",
                node.key
            );
        }
        if let Some(upper) = upper {
            assert!(
                node.key < *upper,
                "invariant 5: {:?} is right of {upper:?}",
                node.key
            );
        }
        if node.color == Color::Red {
            assert!(
                !is_red(&node.left) && !is_red(&node.right),
                "invariant 2: red node {:?} has a red child",
                node.key
            );
        }
        assert!(
            !is_red(&node.right),
            "invariant 4: {:?} has a red right child",
            node.key
        );

        let (left_height, left_count) = check_subtree(node.left.as_deref(), lower, Some(&node.key));
        let (right_height, right_count) =
            check_subtree(node.right.as_deref(), Some(&node.key), upper);
        assert_eq!(
            left_height, right_height,
            "invariant 3: black heights differ below {:?}",
            node.key
        );
        let own_black = usize::from(node.color == Color::Black);
        (left_height + own_black, left_count + right_count + 1)
    }

    fn height<K, V>(node: Option<&Node<K, V>>) -> usize {
        node.map_or(0, |node| {
            1 + height(node.left.as_deref()).max(height(node.right.as_deref()))
        })
    }

    fn entries<K: Clone, V: Clone>(tree: &RedBlackTree<K, V>) -> Vec<(K, V)> {
        tree.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    const KEY_COUNT: u32 = 100;

    /// Orders in which a test inserts or removes the keys `0..KEY_COUNT`.
    #[derive(Clone, Copy, Debug)]
    enum Order {
        Ascending,
        Descending,
        /// Alternates between the smallest and largest keys left: 0, 99, 1, 98, ...
        Zigzag,
        /// A fixed shuffle: multiplying by 37 modulo 100 permutes `0..100`, since 37 and 100
        /// share no factor.
        Shuffled,
    }

    impl Order {
        fn keys(self) -> Vec<u32> {
            match self {
                Self::Ascending => (0..KEY_COUNT).collect(),
                Self::Descending => (0..KEY_COUNT).rev().collect(),
                Self::Zigzag => (0..KEY_COUNT / 2)
                    .flat_map(|i| [i, KEY_COUNT - 1 - i])
                    .collect(),
                Self::Shuffled => (0..KEY_COUNT).map(|i| i * 37 % KEY_COUNT).collect(),
            }
        }
    }

    fn tree_in_order(order: Order) -> RedBlackTree<u32, String> {
        let mut tree = RedBlackTree::new();
        for key in order.keys() {
            tree.insert(key, key.to_string());
        }
        tree
    }

    #[rstest]
    fn an_empty_tree_has_no_entries() {
        let mut tree = RedBlackTree::<u32, &str>::new();

        check_invariants(&tree);
        assert_eq!(tree.len(), 0);
        assert!(tree.is_empty());
        assert_eq!(tree.get(&1), None);
        assert_eq!(tree.get_mut(&1), None);
        assert!(!tree.contains_key(&1));
        assert_eq!(tree.min(), None);
        assert_eq!(tree.max(), None);
        assert_eq!(tree.iter().next(), None);
        assert_eq!(tree.range(..).next(), None);
        assert_eq!(tree.remove(&1), None);
        check_invariants(&tree);
    }

    #[rstest]
    fn inserted_keys_stay_sorted_and_balanced(
        #[values(Order::Ascending, Order::Descending, Order::Zigzag, Order::Shuffled)] order: Order,
    ) {
        let mut tree = RedBlackTree::new();

        for (inserted, key) in order.keys().into_iter().enumerate() {
            assert_eq!(tree.insert(key, key.to_string()), None);
            check_invariants(&tree);
            assert_eq!(tree.len(), inserted + 1);
            assert_eq!(tree.get(&key), Some(&key.to_string()));
        }

        let expected: Vec<_> = (0..KEY_COUNT).map(|key| (key, key.to_string())).collect();
        assert_eq!(entries(&tree), expected);
        assert_eq!(tree.iter().len(), tree.len());
        assert_eq!(tree.min(), Some((&0, &"0".to_owned())));
        assert_eq!(tree.max(), Some((&99, &"99".to_owned())));
        // 2·log2(101) is just over 13, the height bound for 100 nodes.
        assert!(height(tree.root.as_deref()) <= 13);
    }

    #[rstest]
    #[case::same_key_twice(&[(1, "a"), (1, "b")], &[None, Some("a")], &[(1, "b")])]
    #[case::same_key_three_times(
        &[(1, "a"), (1, "b"), (1, "c")],
        &[None, Some("a"), Some("b")],
        &[(1, "c")]
    )]
    #[case::duplicate_among_others(
        &[(2, "a"), (1, "b"), (3, "c"), (1, "d")],
        &[None, None, None, Some("b")],
        &[(1, "d"), (2, "a"), (3, "c")]
    )]
    #[case::same_value_again(&[(1, "a"), (1, "a")], &[None, Some("a")], &[(1, "a")])]
    fn insert_returns_the_value_it_replaces(
        #[case] inserts: &[(u32, &'static str)],
        #[case] expected_returns: &[Option<&str>],
        #[case] expected_entries: &[(u32, &str)],
    ) {
        let mut tree = RedBlackTree::new();

        let returns: Vec<_> = inserts
            .iter()
            .map(|&(key, value)| {
                let previous = tree.insert(key, value);
                check_invariants(&tree);
                previous
            })
            .collect();

        assert_eq!(returns, expected_returns);
        assert_eq!(entries(&tree), expected_entries);
    }

    #[rstest]
    fn removed_keys_are_gone_and_the_tree_stays_balanced(
        #[values(Order::Ascending, Order::Descending, Order::Zigzag, Order::Shuffled)]
        insert_order: Order,
        #[values(Order::Ascending, Order::Descending, Order::Zigzag, Order::Shuffled)]
        remove_order: Order,
    ) {
        let mut tree = tree_in_order(insert_order);
        let mut expected: BTreeMap<_, _> = tree.iter().map(|(k, v)| (*k, v.clone())).collect();

        for key in remove_order.keys() {
            assert_eq!(tree.remove(&key), Some(key.to_string()));
            expected.remove(&key);
            check_invariants(&tree);
            assert_eq!(tree.get(&key), None);
            assert_eq!(
                entries(&tree),
                expected.clone().into_iter().collect::<Vec<_>>()
            );
        }

        assert!(tree.is_empty());
    }

    #[rstest]
    #[case::from_empty_tree(&[], 5)]
    #[case::below_the_smallest_key(&[10, 20, 30], 5)]
    #[case::above_the_largest_key(&[10, 20, 30], 35)]
    #[case::between_keys(&[10, 20, 30], 15)]
    #[case::from_a_single_entry(&[10], 11)]
    fn removing_a_missing_key_changes_nothing(#[case] keys: &[u32], #[case] missing: u32) {
        let mut tree = RedBlackTree::new();
        for &key in keys {
            tree.insert(key, key);
        }
        let before = entries(&tree);

        assert_eq!(tree.remove(&missing), None);

        check_invariants(&tree);
        assert_eq!(entries(&tree), before);
        assert_eq!(tree.len(), keys.len());
    }

    #[rstest]
    fn removing_a_key_twice_returns_nothing_the_second_time() {
        let mut tree = tree_in_order(Order::Shuffled);

        assert_eq!(tree.remove(&42), Some("42".to_owned()));
        assert_eq!(tree.remove(&42), None);

        check_invariants(&tree);
        assert_eq!(tree.len(), 99);
    }

    #[rstest]
    fn get_mut_changes_the_stored_value() {
        let mut tree = tree_in_order(Order::Shuffled);

        if let Some(value) = tree.get_mut(&7) {
            value.push_str(" changed");
        }

        assert_eq!(tree.get(&7).map(String::as_str), Some("7 changed"));
        assert_eq!(tree.get_mut(&KEY_COUNT), None);
    }

    #[rstest]
    fn lookups_accept_borrowed_keys() {
        let mut tree = RedBlackTree::new();
        for key in ["apple", "banana", "cherry", "date"] {
            tree.insert(key.to_owned(), key.len());
        }

        assert_eq!(tree.get("banana"), Some(&6));
        assert!(tree.contains_key("cherry"));
        let in_range: Vec<_> = tree
            .range::<str, _>((Bound::Included("b"), Bound::Excluded("d")))
            .map(|(key, _)| key.as_str())
            .collect();
        assert_eq!(in_range, ["banana", "cherry"]);
        assert_eq!(tree.remove("apple"), Some(5));
        check_invariants(&tree);
    }

    #[rstest]
    #[case::everything((Bound::Unbounded, Bound::Unbounded))]
    #[case::inclusive((Bound::Included(10), Bound::Included(20)))]
    #[case::half_open((Bound::Included(10), Bound::Excluded(20)))]
    #[case::exclusive_start((Bound::Excluded(10), Bound::Included(20)))]
    #[case::from((Bound::Included(95), Bound::Unbounded))]
    #[case::up_to((Bound::Unbounded, Bound::Excluded(3)))]
    #[case::bounds_between_keys((Bound::Included(11), Bound::Included(19)))]
    #[case::single_key((Bound::Included(42), Bound::Included(42)))]
    #[case::below_every_key((Bound::Unbounded, Bound::Excluded(0)))]
    #[case::above_every_key((Bound::Excluded(1000), Bound::Unbounded))]
    fn range_yields_the_same_entries_as_btreemap(#[case] range: (Bound<u32>, Bound<u32>)) {
        // Even keys only, so some bounds fall between keys.
        let mut tree = RedBlackTree::new();
        let mut expected = BTreeMap::new();
        for key in (0..KEY_COUNT).map(|i| i * 2) {
            tree.insert(key, key);
            expected.insert(key, key);
        }

        let actual: Vec<_> = tree.range(range).collect();

        assert_eq!(actual, expected.range(range).collect::<Vec<_>>());
    }

    #[rstest]
    #[case::start_after_end((Bound::Included(20), Bound::Included(10)))]
    #[case::equal_and_excluded((Bound::Excluded(10), Bound::Excluded(10)))]
    fn a_range_that_cannot_hold_keys_is_empty(#[case] range: (Bound<u32>, Bound<u32>)) {
        let tree = tree_in_order(Order::Ascending);

        assert_eq!(tree.range(range).next(), None);
    }

    /// Marsaglia's xorshift64 generator: deterministic and dependency-free, which is all a
    /// reproducible operation sequence needs.
    struct XorShift(u64);

    impl XorShift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: u32) -> u32 {
            u32::try_from(self.next() % u64::from(bound)).unwrap()
        }
    }

    /// Runs the same pseudo-random operations against the tree and a `BTreeMap`, and expects
    /// identical answers. The key space is small relative to the number of operations, so
    /// inserts often overwrite and removals often hit.
    #[rstest]
    fn random_operations_match_btreemap(#[values(1, 42, 0xDEAD_BEEF, u64::MAX)] seed: u64) {
        const OPERATIONS: usize = 5_000;
        const KEY_SPACE: u32 = 300;
        let mut random = XorShift(seed);
        let mut tree = RedBlackTree::new();
        let mut expected = BTreeMap::new();

        for step in 0..OPERATIONS {
            let key = random.below(KEY_SPACE);
            match random.below(10) {
                0..=4 => assert_eq!(tree.insert(key, step), expected.insert(key, step)),
                5..=7 => assert_eq!(tree.remove(&key), expected.remove(&key)),
                8 => assert_eq!(tree.get(&key), expected.get(&key)),
                _ => {
                    let end = key + random.below(KEY_SPACE / 10);
                    assert_eq!(
                        tree.range(key..end).collect::<Vec<_>>(),
                        expected.range(key..end).collect::<Vec<_>>()
                    );
                },
            }
            check_invariants(&tree);
            assert_eq!(tree.len(), expected.len());
        }

        assert!(tree.iter().eq(expected.iter()));
        assert_eq!(tree.min(), expected.first_key_value());
        assert_eq!(tree.max(), expected.last_key_value());
    }
}
