# Adaptive radix tree

A trie that branches on one key byte per level and picks among node types of 4, 16, 48 and 256
children to stay compact. Keys stay in order, and lookups cost the key length rather than the number
of keys.

- **Used by:** DuckDB indexes, HyPer.
- **Cost:** `O(k)` for a `k`-byte key, independent of `n`.
- **Notes:** keys need a binary-comparable encoding (for example, big-endian integers with the sign
  bit flipped). See Leis et al., 2013.

## How it works

### Keys as bytes

A radix tree, or *trie*, never compares whole keys. It reads the key one byte at a time and uses
each byte to choose a child, so a key's position in the tree is its byte string. For the tree's
order to be the key order, keys must be *binary-comparable*: comparing their encodings byte by byte
(as `memcmp` does) must give the same answer as comparing the keys. The paper gives encodings for
common types:

- **Unsigned integers:** big-endian bytes (`to_be_bytes`), most significant byte first.
- **Signed integers:** flip the sign bit, then encode as unsigned, so negative numbers sort first.
- **Strings:** terminate each with a byte that appears nowhere else in any key, such as `0`,
  because no key may be a prefix of another (see Implementation pitfalls).
- **Compound keys:** encode each attribute and concatenate.

### Layout

There are leaves, which hold a value and the full key, and four types of inner node, which differ
only in how they map a key byte to a child. Every inner node starts with a header holding its
type, its number of children and a compressed path (described below).

<figure class="diagram">
{{#include figures/adaptive-radix-tree-nodes.svg}}
<figcaption>The four inner node types, each shown mapping the key bytes <code>0x02</code>, <code>0x41</code> and <code>0x7a</code> to children c0, c1 and c2. Node4 and Node16 keep keys and children in parallel arrays at the same positions. Node48's 256-entry index holds, for each byte value, either nothing or a slot number among its 48 children. Node256 needs no keys at all.</figcaption>
</figure>

A node holds 2 to 4 children as a Node4, 5 to 16 as a Node16, 17 to 48 as a Node48 and 49 to 256
as a Node256. Growing to the next type when one fills up, and shrinking when it empties, keeps
memory proportional to the children actually present. The paper bounds the worst case at 52 bytes
per key, with combined pointer/value slots (see Variants), however long the keys are.

Two techniques remove inner nodes that would only have one child:

- **Lazy expansion:** a leaf is placed as high as possible, as soon as no other key shares its path.
  The remaining bytes of its key are not represented by nodes, so the leaf must store its full key
  (or be able to load it) to confirm a match.
- **Path compression:** an inner node that would have a single child is merged into its child, and
  the skipped bytes are recorded in the child's header as its *prefix*.

<figure class="diagram">
{{#include figures/adaptive-radix-tree-compression.svg}}
<figcaption>The keys "AND\0", "ANT\0", "ANY\0" and "ART\0". A plain trie (left) needs eight inner nodes, one per byte; only the two highlighted ones have more than one child. In the ART (right), path compression folds the root's single "A" into the next node's prefix, and lazy expansion places each leaf as soon as its path is unique, so "ART\0" hangs directly under <code>R</code> with no nodes for "T\0".</figcaption>
</figure>

### Find a child

Given an inner node and a byte `b`:

- **Node4:** compare `b` with each of the up to four keys.
- **Node16:** binary-search the sorted keys, or compare all 16 at once with SIMD and take the
  first matching position.
- **Node48:** read `index[b]`; if it is not empty, it is the slot of the child.
- **Node256:** `children[b]`, which may be empty.

### Search

1. Start at the root with `depth = 0`, the position of the next key byte to consume.
2. If the node is empty, the key is absent. If it is a leaf, compare its full key with the search
   key; equal means found.
3. Compare the node's prefix with the key bytes starting at `depth`. On a mismatch, the key is
   absent. Otherwise, advance `depth` by the prefix length.
4. Find the child for the key byte at `depth`, advance `depth` by one, and continue from step 2.

### Insert

1. If the tree is empty, the new leaf becomes the root.
2. **Reaching a leaf** (lazy expansion): if its key equals the new key, replace the value.
   Otherwise, find how many bytes the two keys share from `depth`, create a Node4 whose prefix is
   those shared bytes, and put the old and the new leaf under it, keyed by the first byte where
   they differ. The Node4 takes the leaf's place.
3. **Prefix mismatch** at an inner node, after `p` matching bytes: create a Node4 with the first
   `p` prefix bytes as its prefix. Add the existing node under the prefix byte where they differ,
   and shorten that node's prefix to the bytes after it (`prefix_len - (p + 1)` of them). Add the
   new leaf under the key byte where they differ. The Node4 takes the node's place.
4. Otherwise, advance `depth` past the prefix and find the child for the next key byte. If there
   is one, recurse into it with `depth + 1`.
5. If there is no such child, add the new leaf here. If the node is full, first replace it with
   the next larger type, copying its header and children.

### Delete

1. Find the leaf as in a search, keeping track of its parent, and remove it from the parent.
2. If the parent now has few enough children for a smaller type, replace it with one.
3. If the parent is left with a single child, remove the parent: the child takes its place, and
   the child's new prefix is the parent's prefix, then the byte that led to the child, then the
   child's old prefix. A single remaining leaf takes the parent's place as it is.

### Range scan

Children in every node type can be visited in byte order (a Node48 by walking its 256-entry
index), so an in-order traversal returns keys in sorted order. A scan from a start key descends as
in a search, then continues to the next larger child at each level once the path leaves the
start key.

### Variants

- **Prefix storage.** *Pessimistic* path compression stores the whole skipped prefix in each node
  and checks it during search. *Optimistic* compression stores only its length, skips the bytes
  and relies on the final leaf comparison to catch a wrong turn. The paper uses a hybrid: up to 8
  prefix bytes are stored and checked, and longer prefixes fall back to the optimistic check.
- **Leaf representation.** *Single-value leaves* are separate nodes, which allows keys and values
  of any length. *Multi-value leaves* mirror the inner node types but need fixed-length keys.
  *Combined pointer/value slots* store a value directly in a child slot when it fits, told apart
  from a pointer by a tag bit; the paper recommends this when it applies, for example for
  secondary indexes whose values are tuple identifiers.
- **Concurrency.** Leis et al., 2016 make the tree concurrent with optimistic lock coupling and
  with ROWEX (read-optimised write exclusion).

## Invariants

- No stored key is a prefix of another.
- Every inner node has at least two children (the root may be a leaf or empty instead).
- Each inner node's child count fits its type, and is above the threshold at which it would shrink.
- Node4 and Node16 keys are sorted and distinct, and only the first `count` entries are used.
- In a Node48, exactly `count` index entries are non-empty, and they point at distinct, occupied
  child slots.
- For every leaf, the path to it (prefixes and branch bytes, root to leaf) is a prefix of its key.
- An in-order traversal yields keys in strictly increasing byte order.

## Implementation pitfalls

- **Keys that are prefixes of other keys.** With `"a"` and `"ab"` in the tree, `"a"` would have
  to live at an inner node where the path for `"ab"` continues. A terminator byte avoids it only if
  that byte cannot occur inside a key; for arbitrary binary keys, escape the terminator, or store
  an optional value in inner nodes for the key that ends there.
- **Native byte order.** `to_ne_bytes` on a little-endian machine puts the least significant byte
  first, and integers then sort in a scrambled order. Always encode big-endian.
- **Floating-point keys.** Raw IEEE 754 bits do not sort correctly for negative numbers; the paper
  ranks them by class (negative or positive, normal, subnormal, zero, infinity, NaN) first.
- **Prefixes longer than the stored bytes.** With optimistic or hybrid compression, a search can
  reach a leaf that does not match. Every search has to end with a full-key comparison, and an
  insert that splits a long prefix has to load the missing bytes from a leaf below the node.
- **Short keys.** The key may run out before the tree does. Read bytes with `get(depth)` and treat
  `None` as a mismatch; depth and prefix arithmetic should use `checked_add` and `checked_sub`.
- **Node48 holes.** Removing a child leaves an unused slot among the 48. Either search for a free
  slot on insert or move the last child into the hole and update its index entry.
- **Node representation in Rust.** An enum with a variant per node type is natural; box the large
  variants so the enum stays small. Build arrays of non-`Copy` children with `std::array::from_fn`,
  and replace a node in its parent's slot with `std::mem::replace` when it grows or shrinks.
- **SIMD without `unsafe`.** The SSE intrinsics in the paper need `unsafe`, which this crate
  forbids. A binary search over sixteen bytes, or a loop the compiler may vectorise, is the safe
  substitute.
- **Grow and shrink thrashing.** If a node shrinks at exactly the count at which it grows,
  alternating inserts and deletes at that boundary reallocate every time. Shrinking at a lower
  count than the growth point avoids it.
- **Recursion depth.** It is bounded by the key length rather than by `log n`, so very long keys
  can recurse deeply. An iterative descent avoids the problem.

## Testing

- Reference model: a `BTreeMap<Vec<u8>, V>` driven by the same random inserts, removes, lookups
  and range scans.
- Shared prefixes: draw keys from a small alphabet so that many share long prefixes, and include
  prefixes longer than the 8 stored bytes to exercise the optimistic path.
- Node transitions: under one parent, insert children one byte value at a time from 1 up to 256,
  then remove them again, checking contents and node types at each count where the type changes.
- Encoding properties: for random pairs of integers (signed and unsigned) and strings, assert that
  `a < b` exactly when `encode(a) < encode(b)`.
- Structure checks: walk the tree after each operation and assert the invariants above.
- Edge cases: the empty tree, a single key, the empty key, keys that differ only in their last byte,
  and deleting down to one key and then to none.

## Further reading

- [The Adaptive Radix Tree: ARTful Indexing for Main-Memory
  Databases](https://db.in.tum.de/~leis/papers/ART.pdf), Leis, Kemper and Neumann, 2013.
- Leis et al., *The ART of Practical Synchronization*, 2016. Making the tree concurrent with
  optimistic lock coupling.
