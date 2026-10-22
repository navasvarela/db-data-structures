# Skip list

> In this crate: `crossbeam_skiplist::SkipMap` inside `backend::memory::MemoryByteStore`, in
> [`src/backend/memory.rs`](https://github.com/navasvarela/db-data-structures/blob/main/src/backend/memory.rs).

A sorted linked list with extra, randomly promoted "express lane" levels. It keeps keys ordered like
a balanced tree, but a lock-free version is far simpler to write than a lock-free tree.

- **Used by:** LevelDB and RocksDB memtables, Redis sorted sets.
- **Cost:** expected `O(log n)` search, insert and delete (Pugh, 1990).
- **Notes:** a hand-written single-threaded version would make a good companion to the red-black
  tree, to compare the two as memtables.

## How it works

### Layout

A skip list is a sorted linked list in which every node also carries a *tower* of extra forward
links. A node of *height* `h` is linked into levels `0` to `h - 1`. Level 0 is the ordinary
sorted list of every node; each level above it links a subset of the level below, so a search
can travel along a high level, skipping many nodes, and drop down a level when it would overshoot.

<figure class="diagram">
{{#include figures/skip-list-layout.svg}}
<figcaption>A skip list of twelve keys. 31 has height 4, 9 and 52 have height 3, and 3, 25 and 40 have height 2; every other node is on level 0 only. The grey tower on the left is the head.</figcaption>
</figure>

A *head* node with no key and a tower of the maximum height starts every level. Heights are
random, not balanced by rules: when a node is created, its height is drawn so that

```text
P(height ≥ i + 1) = p^i        for a fixed p, usually 1/2 or 1/4
```

so about a fraction `p` of the nodes on each level also appear on the next one. Pugh caps the
height at `log_{1/p}(N)` levels for an expected maximum of `N` nodes. LevelDB uses `p = 1/4` and a
maximum height of 12. The list also tracks its current height, the tallest tower in use, so a
search does not start on empty levels.

### Search

1. Start at the head, on the highest level in use.
2. While the next node on this level has a key smaller than the target, move to it.
3. When the next node's key is not smaller (or there is none), drop one level and repeat step 2.
4. After level 0, the next node is the first one whose key is at least the target. It is a match
   if its key equals the target.

<figure class="diagram">
{{#include figures/skip-list-search.svg}}
<figcaption>Searching for 28. On level 3 the next key, 31, is too large, so the search drops at once. It moves to 9 on level 2, to 25 on level 1, and reaches 28 on level 0: three forward moves, where level 0 alone would take seven.</figcaption>
</figure>

The same walk, stopping at step 4 without checking equality, is a *seek*: the starting point of a
range scan, which then follows level-0 links.

### Insert

1. Search as above, but record, on every level, the last node visited before dropping down. These
   are the *predecessors*, kept in an array `update[0..max_height]`.
2. If the node after `update[0]` holds the key, replace its value and stop.
3. Draw a height `h` for the new node. If `h` is above the current height, set `update[i]` to the
   head for the new levels and raise the current height.
4. For each level `i` below `h`: point the new node's `next[i]` at `update[i].next[i]`, then point
   `update[i].next[i]` at the new node.

<figure class="diagram">
{{#include figures/skip-list-insert.svg}}
<figcaption>Inserting 20 with a drawn height of 2. The search records <code>update[3]</code> = head, <code>update[2]</code> = <code>update[1]</code> = 9 and <code>update[0]</code> = 17, the predecessors. Only levels 0 and 1 change: 20 is spliced in after 17 and after 9.</figcaption>
</figure>

### Delete

1. Search recording predecessors, as for an insert.
2. If the node after `update[0]` does not hold the key, stop.
3. For each level `i` in the node's tower, point `update[i].next[i]` at the node's `next[i]`.
4. While the highest level in use has no nodes, lower the current height.

A memtable never deletes: a delete is written as a tombstone entry (see
[memtable](../lsm-trees/memtable.md)), and the whole skip list is dropped after it has been flushed.
LevelDB's skip list has no delete operation at all.

### Concurrent access

A skip list is popular in memtables because concurrency is comparatively easy:

- **One writer, lock-free readers (LevelDB).** Writers are serialised by a mutex. An insert
  fills in the new node's own links first, then publishes it by storing it into each
  predecessor's link with release ordering, from level 0 upwards; readers load links with
  acquire ordering. A reader therefore sees either the old list or a fully initialised node, and
  nodes are never freed while the list is alive.
- **Fully lock-free (crossbeam).** Several writers splice nodes in with compare-and-swap, and
  deletions mark a node's links before unlinking it. Removed nodes can only be freed once no
  thread can still hold a pointer to them, which `crossbeam_skiplist` solves with epoch-based
  reclamation from `crossbeam-epoch`.

### Variants

- **Indexable skip list:** each link also stores its *span*, the number of level-0 nodes it
  skips. Summing spans along a search gives a node's rank, which is how Redis sorted sets answer
  rank queries.
- **Deterministic skip lists:** heights are maintained by rules instead of coin flips, which
  makes them equivalent to B-trees of small order. They give worst-case bounds but lose the
  simplicity that makes skip lists attractive.

## Invariants

- Level 0 contains every node, in strictly increasing key order.
- Every level is a subsequence of the level below it, in the same order.
- A node of height `h` is linked into exactly levels `0` to `h - 1`.
- Every height lies between 1 and the maximum height, and the head's tower has the maximum height.
- No level above the current height has any node.
- `len` equals the number of nodes on level 0.

## Implementation pitfalls

- **Shared ownership of nodes.** A tall node is pointed to by a predecessor on every level of its
  tower, so no single link can own it with a `Box`. Without `unsafe`, the practical choice is an
  arena: a `Vec` of nodes addressed by index, with links stored as `Option<usize>` (or a newtype
  around `usize`) and the head at a fixed index. `Rc<RefCell<_>>` also works, but costs a
  reference count and a runtime borrow check per step.
- **Freed slots in an arena.** If deletion recycles arena slots through a free list, an index
  held by an iterator can come to point at a different node. A memtable that never deletes
  avoids the problem; otherwise, add a generation counter to each slot.
- **Random heights.** A height of `1 + trailing_zeros(random u64)`, capped, gives `p = 1/2`; for
  `p = 1/4`, count trailing pairs of zero bits. Forgetting the cap lets a rare draw build a huge
  tower, and an unseeded generator makes failing tests impossible to replay: inject a seedable
  generator.
- **Predecessor array bounds.** Size `update` at the maximum height, not the current one, since an
  insert may raise the height. Iterate levels with `(0..height).rev()` instead of decrementing a
  counter, which underflows below zero; with this crate's lints, `get` and `get_mut` replace
  `next[level]`.
- **Duplicate keys.** The algorithms assume distinct keys. A memtable that keeps several versions
  of a key gets uniqueness by sorting on the user key plus a sequence number.
- **Publication order in a concurrent list.** Linking the node into a predecessor before setting
  its own `next` pointers lets a reader follow an uninitialised link. In safe Rust this shows up
  as needing atomics and a reclamation scheme, which is the reason to use `crossbeam_skiplist`
  rather than write one.
- **Recursive drop.** If you do use owning `Box` links on level 0, dropping a long list recurses
  once per node and can overflow the stack; an arena or an explicit loop in `Drop` avoids it.

## Testing

- Reference model: run random insert, get, remove and range operations against the skip list
  and a `BTreeMap`, and compare every answer, including the full iteration order.
- Structure checks after each operation: the invariants above, walked level by level.
- Height distribution: after many inserts, check that the fraction of nodes with height at least
  `i` is close to `p^(i - 1)`.
- Determinism: with the same seed, two lists built from the same operations have identical towers.
- Ordered input: insert keys in ascending and in descending order, which degenerates a naive
  binary search tree but should not affect a skip list.
- Concurrency, for a concurrent list: several threads inserting distinct keys while others
  iterate; every iteration must be sorted, and the final contents must be the union of all
  inserts.

## Further reading

- [Skip Lists: A Probabilistic Alternative to Balanced
  Trees](https://www.epaperpress.com/sortsearch/download/skiplist.pdf), Pugh, 1990. Short and
  readable, with pseudocode.
- [Mini-LSM](https://skyzh.github.io/mini-lsm/), Chi. Its first week builds a memtable on a skip
  list.
