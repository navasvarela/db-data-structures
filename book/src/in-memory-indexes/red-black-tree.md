# Red-black tree

A binary search tree that stays balanced by colouring nodes and rotating them on insert and delete.
It keeps keys in order and supports range scans.

- **Used by:** ordered in-memory maps in many engines; a common memtable alternative to the skip
  list.
- **Cost:** worst-case `O(log n)` search, insert and delete; height at most `2·log2(n + 1)`.
- **Notes:** the branch implements the left-leaning variant (Sedgewick, 2008), which avoids parent
  pointers and so fits safe Rust.

## How it works

### Layout

Each node holds a key, a value, a colour (red or black) and links to its two children. On the
`red-black-tree` branch a link is an `Option<Box<Node>>`, so each node owns its subtree and there
are no parent links. Empty links count as black.

The colours make sense through the *2-3 tree* the red-black tree encodes. A 2-3 tree has
*2-nodes* (one key, two children) and *3-nodes* (two keys, three children), and keeps every leaf
at the same depth. A red node is the smaller key of a 3-node, glued to its parent; black nodes are
the 2-nodes and the larger keys of 3-nodes. The *left-leaning* variant (LLRB) also requires every
red node to be a left child, so each 3-node has exactly one shape.

<figure class="diagram">
{{#include figures/red-black-tree-2-3.svg}}
<figcaption>A 2-3 tree and the left-leaning red-black tree that encodes it. The 3-node <code>E J</code> becomes a black J with a red left child E; red nodes and the links into them are drawn in red. Every path from the root down crosses three black nodes, as every leaf of the 2-3 tree is at depth three.</figcaption>
</figure>

Every path from the root to an empty link crosses the same number of black nodes, its *black
height*, because those are the levels of the 2-3 tree. Red nodes add at most one extra step per
black node, which bounds the height by `2·log2(n + 1)`.

### Search

Search ignores colours: compare with the node's key and go left, go right, or stop on equality.
Range scans walk the tree in order with an explicit stack, which the branch's `InOrder` iterator
does: push the left spine from the start bound, then, after visiting a node, push the left spine
of its right subtree.

### The three repair operations

Insertion and deletion are built from three local operations, none of which changes a black
height or the order of keys:

<figure class="diagram">
{{#include figures/red-black-tree-repairs.svg}}
<figcaption><code>rotate_left(h)</code>: the new top, x, takes h's colour, and h becomes red; the subtrees a, b and c keep their order. <code>rotate_right(h)</code> is the mirror image. <code>flip_colours(h)</code> switches the colour of h and both children: read left to right, it splits a 4-node and passes the middle key up; right to left, it merges h with its children into a 4-node.</figcaption>
</figure>

### Insert

The branch inserts recursively and repairs each node on the way back up:

1. Descend as in a search. If the key is found, replace the value and return the old one.
2. At the empty link where the key belongs, create a new **red** node. A red node never changes a
   black height, so only the red-red and left-leaning rules can break.
3. On the way back up, at each node `h`, apply `fix_up`, in this order:
   1. if `h.right` is red and `h.left` is black, `rotate_left(h)`;
   2. if `h.left` and `h.left.left` are both red, `rotate_right(h)`;
   3. if both children are red, `flip_colours(h)`.
4. Colour the root black. If the last flip made it red, this grows every black height by one,
   which is how the tree gets taller.

Each step can create the shape that the next step fixes, which is why the order matters. Doing
the colour flip last, on the way up, makes the tree a 2-3 tree; Sedgewick's paper shows that
doing it on the way down instead gives a 2-3-4 tree.

<figure class="diagram">
{{#include figures/red-black-tree-insert.svg}}
<figcaption>Inserting A, C and B into an empty tree. C arrives as a red right child, which <code>fix_up</code> rotates left (1–2). B arrives as a red right child of A (3): rotating A left (4) leaves two reds in a row on the left, which rotating C right turns into a node with two red children (5). The flip makes B red, and colouring the root black finishes the insert (6).</figcaption>
</figure>

### Delete

Deleting a black node would shorten one path's black height, so deletion makes sure the node it
finally removes is red. On the way down, it never steps into a 2-node: before moving to a child,
it borrows a red link from the child's sibling, or merges the child with its sibling and parent.

- `move_red_left(h)`, used when `h.left` and `h.left.left` are both black: flip colours; then, if
  `h.right.left` is red, `rotate_right(h.right)`, `rotate_left(h)` and flip colours again.
- `move_red_right(h)`, used when `h.right` and `h.right.left` are both black: flip colours; then,
  if `h.left.left` is red, `rotate_right(h)` and flip colours again.

Removing the smallest key, the building block:

1. If `h` has no left child, it is a leaf (the left-leaning and black-height rules allow nothing
   else); remove it.
2. Otherwise, if `h.left` and `h.left.left` are both black, `move_red_left(h)`.
3. Recurse into the left child, then `fix_up(h)` on the way back.

Removing any key:

1. If the key is smaller than `h.key`: `move_red_left(h)` if needed, recurse left.
2. Otherwise:
   1. if `h.left` is red, `rotate_right(h)`, so the key can be reached on the right;
   2. if the key equals `h.key` and `h` has no right child, `h` is a leaf; remove it;
   3. if `h.right` and `h.right.left` are both black, `move_red_right(h)`;
   4. if the key equals `h.key`, replace `h`'s entry with the smallest entry of its right subtree,
      and remove that smallest entry from the right subtree; otherwise, recurse right.
3. Apply `fix_up(h)` on the way back up, and colour the root black at the end.

The descent reshapes the tree before it knows whether the key is present, so the branch checks
`contains_key` first and never starts a removal of a missing key.

### Variants

- **Classic red-black tree** (Cormen et al.; Guibas and Sedgewick, 1978): red nodes may lean
  either way, so it is a 2-3-4 tree. Repairs walk up from the changed node using parent links,
  with more cases than the left-leaning variant, but at most a constant number of rotations per
  insert or delete; the left-leaning version may rotate at every level of the search path.
- **AVL tree:** balances on subtree heights instead of colours. It is more strictly balanced, so
  lookups are slightly shorter and updates do more rebalancing.

## Invariants

These are the invariants the branch's `check_invariants` test helper asserts:

- **Black root:** the root is black.
- **No red-red:** a red node never has a red child.
- **Equal black height:** every path from a node to an empty link crosses the same number of
  black nodes.
- **Left-leaning:** a red node is always a left child.
- **Search order:** every key in a node's left subtree is smaller than the node's key, and every
  key in its right subtree is larger.
- `len` equals the number of nodes, and the height is at most `2·log2(n + 1)`.

## Implementation pitfalls

- **Ownership during rotations.** A rotation moves a node under its own child, which a borrow of
  the tree cannot express. Write the helpers to take a `Box<Node>` by value and return the new
  subtree root, detaching children with `Option::take` and putting them back afterwards, as
  `rotate_left` and `insert_into` on the branch do.
- **Parent links.** The classic algorithm needs them, which in safe Rust means `Rc` and `Weak`, or
  an arena of nodes addressed by index where every lookup returns an `Option` that has to be
  handled, because this crate denies panics. Avoiding them is the reason for the left-leaning
  variant.
- **Rotations that forget colours.** The new top must take the old top's colour and the old top
  must become red; otherwise black heights change and the tree drifts out of balance with no
  visible error until the invariant check runs.
- **Wrong `fix_up` order.** Rotating right before rotating left misses a red right child created
  by the flip below it.
- **Removing a missing key.** The descent's colour flips have nothing to absorb them below the
  last node, breaking the black height. Check for the key first.
- **Successor swap.** When an inner node takes over its successor's entry, the value returned to
  the caller must be the inner node's original value, not the successor's.
- **Recursion depth.** Recursive insert and delete go as deep as the tree, which the height bound
  keeps below 128 levels for any `usize` length, so recursion is safe here. Recursive drop of the
  boxes is bounded the same way.
- **Range bounds.** `BTreeMap::range` panics when the start is after the end; with this crate's
  lints, return an empty iterator instead, as the branch does.
- **Inconsistent `Ord`.** A key type whose `Ord` disagrees with its `Eq`, or a `Borrow` form that
  orders differently from the owned key, sends lookups down the wrong subtree.

## Testing

- Invariant checker: a recursive function that returns each subtree's black height and node
  count, asserting the invariants above at every node, called after every operation in every test.
- Insertion orders: ascending, descending, alternating smallest and largest, and shuffled, since
  ascending input is what makes an unbalanced tree degenerate.
- Removal orders: every combination of the insertion orders above with the same removal orders,
  checking contents against a `BTreeMap` after each removal.
- Reference model: long seeded sequences of random inserts, removes, gets and range scans against
  a `BTreeMap`, with a key space small enough that overwrites and hits are common.
- Height bound: after building a tree of `n` keys, assert the height is at most `2·log2(n + 1)`.
- Edge cases: the empty tree, a single node, removing a missing key, removing the same key twice,
  and ranges with every combination of included, excluded and unbounded ends.

## Further reading

- [Left-leaning Red-Black
  Trees](https://sedgewick.io/wp-content/themes/sedgewick/papers/2008LLRB.pdf), Sedgewick, 2008. The
  variant on the `red-black-tree` branch.
- Sedgewick and Wayne, *Algorithms*, 4th ed., section 3.3, which builds red-black trees up from 2-3
  trees.
- [Learn Rust With Entirely Too Many Linked
  Lists](https://rust-unofficial.github.io/too-many-lists/). How ownership shapes linked structures
  in Rust; worth reading before writing a tree in safe code.
