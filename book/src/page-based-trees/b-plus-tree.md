# B+ tree

> Builds on [slotted page](../storage-primitives/slotted-page.md) and [buffer
> pool](../storage-primitives/buffer-pool.md).

A balanced tree of pages with high fan-out. Inner pages hold separator keys, leaf pages hold the
entries, and leaves are linked for range scans. With hundreds of keys per page, the tree is rarely
more than four levels deep.

- **Used by:** InnoDB clustered and secondary indexes, PostgreSQL B-tree indexes, SQLite tables and
  indexes, most relational databases.
- **Cost:** `O(log_B n)` page reads for lookup, insert and delete, where `B` is the fan-out.
- **Notes:** splits and merges must be logged to the [WAL](../storage-primitives/write-ahead-log.md)
  to be crash-safe. Bayer and McCreight, 1972.

## How it works

### Layout

The tree lives in a file of fixed-size pages, addressed by page ID, and read through the
[buffer pool](../storage-primitives/buffer-pool.md). Page 0 is usually a *meta page* that records
the root's page ID, the tree height and the head of the free-page list. Every other page is a node,
laid out as a [slotted page](../storage-primitives/slotted-page.md) so that variable-length keys can
be inserted in sorted order without moving the bytes of the others.

There are two kinds of node:

- An **inner page** holds `n` separator keys and `n + 1` child page IDs. Child `i` covers the keys
  `k` with `K_i ≤ k < K_(i+1)`, where `K_0` is minus infinity and `K_(n+1)` is plus infinity. A
  common encoding stores the leftmost child ID in the page header and `(K_i, child_i)` pairs in the
  slots.
- A **leaf page** holds `(key, value)` entries in key order, plus the page ID of its right sibling.
  The value is the row itself in a clustered index, or a row ID in a secondary index.

<figure class="diagram">
{{#include figures/b-plus-tree-layout.svg}}
<figcaption>A tree of height 2. Grey cells are child page IDs: <code>c3</code> is page 3. Page 5 holds the keys from 40 up to, but not including, 80.</figcaption>
</figure>

The page header also records the node type, the number of slots and the free-space bounds. Storing
the level (0 for leaves, counting up) in every page costs a byte and makes corruption checks and
some algorithms simpler, because a page can tell where it sits without a parent pointer.

The fan-out `B` is how many separators fit in an inner page. With 8 KiB pages and 16-byte entries
it is several hundred, so three inner levels already address tens of millions of leaves.

### Search

1. Read the root page ID from the meta page.
2. In an inner page, binary-search for the last separator `K_i ≤ key`, and follow child `i`. If
   every separator is greater than the key, follow the leftmost child.
3. Repeat until a leaf, then binary-search the leaf for the key.

Each level costs one page read, so a lookup costs the tree height in page reads, and in practice
the upper levels stay in the buffer pool.

### Insert

1. Descend as in search, and push each inner page ID and the child index taken onto a path stack.
2. Insert the entry into the leaf at its sorted position. If it fits, write the page and stop.
3. Otherwise split the leaf, as below, which produces a separator key and a new right page.
4. Pop the parent from the path stack and insert `(separator, new page ID)` immediately after the
   child index recorded on the way down. If the parent overflows, split it too, and continue up.
5. If the root splits, allocate a new root with two children and the one separator, and update the
   root ID and height in the meta page. This is the only way the tree grows taller, which is why
   every leaf stays at the same depth.

### Split

A **leaf split**:

1. Allocate a new page.
2. Choose a split point that divides the bytes, not the entry count, roughly in half, counting the
   entry being inserted. Move the entries above the split point to the new page.
3. Set the new page's right sibling to the old page's right sibling, then set the old page's right
   sibling to the new page.
4. *Copy* the first key of the new page up to the parent as the separator. It stays in the leaf,
   since leaves must hold every key.

<figure class="diagram">
{{#include figures/b-plus-tree-leaf-split.svg}}
<figcaption>Inserting 60 into the full page 5, with three entries per leaf. The new page 11 takes 60 and 70 and the old right link, page 5 links to it, and <code>(60, c11)</code> goes into the parent right after <code>c5</code>.</figcaption>
</figure>

An **inner split** is the same, except that the middle separator is *moved* up rather than copied:
it becomes the parent's separator, and its child pointer becomes the new page's leftmost child.
Inner pages have no sibling links in a basic B+ tree.

<figure class="diagram">
{{#include figures/b-plus-tree-inner-split.svg}}
<figcaption>An inner split. The middle separator 60 leaves page 12 for the parent, and the child to its right, <code>c4</code>, becomes the leftmost child of the new page 13.</figcaption>
</figure>

A split changes at least three pages (the old page, the new page and the parent), so the whole
change must be logged to the [WAL](../storage-primitives/write-ahead-log.md) as one atomic unit.
A crash between the page writes otherwise leaves a page that nothing points to, or a separator that
points to a page that was never written.

### Delete and underflow

1. Descend to the leaf, recording the path, and remove the entry.
2. If the leaf is still at least the minimum fill (classically half full), stop.
3. Otherwise look at an adjacent sibling *with the same parent*:
   - **Borrow:** if the sibling has entries to spare, move one or more of them across, and update
     the separator between the two pages in the parent.
   - **Merge:** otherwise move all entries of the right page into the left one, fix the left page's
     sibling link, remove the separator and the right child from the parent, and free the right
     page.
4. A merge removes an entry from the parent, which can underflow in turn; repeat up the path. For an
   inner merge, the separator from the parent is pulled down between the two halves.
5. If the root is an inner page left with a single child, make that child the root and decrement
   the height.

Many production trees relax this. PostgreSQL never merges partly full pages and only removes pages
that have become completely empty, and InnoDB merges a page with a neighbour only once it falls
below a configurable threshold (`MERGE_THRESHOLD`, 50% by default). Lazy deletion trades space
for simpler code and fewer page writes; it is a reasonable first implementation as well.

### Range scan

1. Descend to the leaf that would hold the start key, and binary-search for the first entry at or
   after it.
2. Return entries in order. At the end of a leaf, follow its right-sibling link.
3. Stop at the first key past the end bound, or at a leaf with no right sibling.

Only one leaf needs to be pinned at a time, and the inner pages are not touched after the first
descent.

### Bulk load

Inserting sorted keys one at a time leaves every leaf half full, since each split leaves the left
half behind. When the data is already sorted, for example when an index is created on an existing
table, build the tree bottom-up instead:

1. Fill leaves left to right up to a chosen fill factor, linking each to the next.
2. Record the first key and page ID of every leaf except the first.
3. Build the next level the same way from those `(key, page ID)` pairs, and repeat until a level
   has a single page. That page is the root.

### Variants

- **B-tree (without the plus):** values live in inner nodes too, so a lookup can stop early, but
  range scans have to walk the whole tree instead of a leaf chain.
- **Suffix truncation and prefix compression:** a separator only has to tell two neighbouring
  pages apart, so it can be cut down to the shortest prefix that does so. Separately, keys within
  one page often share a common prefix, which can be stored once. Both raise the fan-out.
- **Overflow pages:** values too large for a page are stored in a chain of separate pages, and the
  leaf keeps a pointer. SQLite does this for large rows.
- **Concurrency:** the [B-link tree](b-link-tree.md) adds sibling links and high keys to every
  level so readers do not need to hold a lock on the parent while they read the child.

## Invariants

- Keys within every page are strictly increasing (or non-decreasing, if duplicates are allowed and
  handled as described below).
- Every inner page with `n` separators has exactly `n + 1` children.
- Every key in the subtree of child `i` satisfies `K_i ≤ key < K_(i+1)` for that child's bounds in
  its parent; this holds recursively, with the bounds narrowing on the way down.
- All leaves are at the same depth, equal to the height recorded in the meta page, and every page's
  stored level matches its distance from the leaves.
- Following right-sibling links from the leftmost leaf visits every leaf exactly once, and yields
  all keys in increasing order, the same sequence as an in-order traversal of the tree.
- Every non-root page is at least the minimum fill (if the implementation merges), and an inner
  root has at least two children.
- Every allocated page is either reachable from the root exactly once, or on the free list; never
  both, never neither.

## Implementation pitfalls

- **Off-by-one separator semantics.** Search, split and merge must agree on whether a separator
  equal to the key belongs to the left or the right child. A split that copies up the first key of
  the new page needs a search that goes right on equality. Pick one convention, write it down next
  to the search code, and test keys equal to separators explicitly.
- **Duplicate keys.** If equal keys can span two leaves, a search that lands in the right one
  misses entries in the left. The usual fix is to make every key unique by appending the row ID,
  as PostgreSQL does with the heap tuple ID.
- **Splitting by count with variable-length keys.** Half the entries can be far more than half the
  bytes, and the entry being inserted may still not fit on its side after the split. Split by
  bytes, and include the new entry when choosing the split point.
- **Keys that are too large.** A split needs at least two entries per page, and a meaningful
  separator. Put a maximum on the key size (PostgreSQL limits index entries to about a third of a
  page) or move large values to overflow pages.
- **Parent pointers.** Storing a parent page ID in every page looks convenient, but an inner split
  then has to rewrite the parent pointer of every child that moved, which is hundreds of page
  writes. Keep the path from the descent in a stack instead. In Rust this also avoids references
  between pages: nodes refer to each other only by page ID, and only the buffer pool owns page
  memory.
- **Two pages mutably at once.** A split or merge edits two or three pages together. A buffer pool
  that hands out `&mut` borrows of its frame array will not allow that. Design it to hand out
  separate guard objects per pinned page, so that holding two of them is natural.
- **Unchecked arithmetic in page code.** Slot offsets, free-space calculations and split points are
  all arithmetic on page-relative offsets. In this crate, clippy denies plain arithmetic that can
  overflow and slice indexing, so use `checked_sub` and `checked_add` and `get` throughout, and
  treat a failure as a corruption error rather than panicking.
- **Freed pages.** A page freed by a merge must not be reused until nothing can reach it, and it
  must reach the free list in the same atomic WAL unit as the merge, or it leaks after a crash.
- **Recursive algorithms.** Recursion is fine for the descent, since the height is small, but keep
  the split-propagation loop iterative over the path stack; it makes the WAL logging of each step
  easier to reason about.

## Testing

- **Model test.** Run random sequences of insert, delete, get and range-scan against the tree and
  against `std::collections::BTreeMap`, and assert they always agree. A property-testing crate
  such as `proptest` shrinks a failing sequence to a short one.
- **Structural validator.** Write a function that walks the whole tree and checks every invariant
  above, and call it after every operation in the model test.
- **Tiny pages.** Make the page size (or the maximum entries per page) a parameter, and run the
  tests with pages that hold three or four entries, so that splits, merges and root changes happen
  within a few dozen operations.
- **Adversarial orders.** Insert ascending, descending and interleaved keys; delete everything and
  reinsert; delete from the left edge repeatedly so that merges cascade up to the root.
- **Range scan bounds.** Scan with bounds equal to existing keys, between keys, before the first and
  after the last key, and across leaf boundaries.
- **Persistence and crashes.** Close and reopen the file and rerun the model checks. With a WAL,
  use a page store that can drop all writes after a chosen point to simulate a crash in the middle
  of a split, then recover and validate.

## Further reading

- [Organization and Maintenance of Large Ordered Indexes](https://doi.org/10.1007/BF00288683), Bayer
  and McCreight, 1972.
- [The Ubiquitous B-Tree](https://doi.org/10.1145/356770.356776), Comer, 1979. A short survey that
  explains why B+ trees won.
- [Modern B-Tree Techniques](https://nowpublishers.com/article/Details/DBS-028), Graefe, 2011. The
  comprehensive reference; *More Modern B-Tree Techniques* (2024) follows it up.
- Petrov, *Database Internals*, O'Reilly, 2019, chapters 2 and 4, "B-Tree Basics" and "Implementing
  B-Trees".
- [Build Your Own Database From Scratch](https://build-your-own.org/database/), Ye. A disk-based B+
  tree written step by step; the first part is free online.
