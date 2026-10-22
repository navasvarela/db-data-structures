# B-link tree

> Builds on [B+ tree](../page-based-trees/b-plus-tree.md).

A B+ tree in which every page also links to its right sibling and stores a high key. A reader that
lands on a page mid-split can follow the link to the right instead of waiting for a lock, so readers
never block writers.

- **Used by:** PostgreSQL's `nbtree`.
- **Notes:** Lehman and Yao, 1981.

## How it works

### Layout

Pages are laid out as in a [B+ tree](b-plus-tree.md), with two additions on *every* page, inner
pages included:

- a **right link**: the page ID of the next page to the right on the same level;
- a **high key**: an upper bound on the keys the page may hold. Every key on the page is at most
  the high key, and every key on the right sibling is greater than it. The rightmost page of a level
  has no high key; it is implicitly plus infinity.

The point of these two fields is that a page split can be done in two separate steps. After the
first step, the new right page has no downlink in the parent yet, but it is still reachable through
its left sibling's right link. This intermediate state is called a *half split*.

<figure class="diagram">
{{#include figures/b-link-tree-half-split.svg}}
<figcaption>A leaf split in two steps. <code>hk</code> is the high key, and the dot on the right of each page is its right link. In state 2, <code>Q</code> has no downlink yet, but it is reachable through the right link of <code>P</code>.</figcaption>
</figure>

A reader that followed the old downlink to `P` looking for `75` finds `75 > 60`, the new high key,
and moves right to `Q`. No separator in the parent has to be correct at the moment of reading; it
only has to send the reader to a page at or to the left of the right one.

Lehman and Yao's paper assumes readers need no locks at all. Real systems, PostgreSQL among them,
share page buffers between processes, so readers take a short shared *latch* (a lightweight lock
that protects a page's bytes for the duration of one access, as opposed to a transaction lock on
rows) on each page while examining it. The key property survives: a reader never holds more than
one latch, and never waits for a structure change to finish.

### Search

1. Start at the root. Latch the page in shared mode.
2. **Move right:** while the page is not rightmost and the search key is greater than its high key,
   remember the right link, release the latch, and latch the right sibling.
3. In an inner page, pick the child as in a B+ tree, release the latch, then latch the child.
4. Repeat until a leaf, applying the move-right check on every page, including the leaf.

The latch on the parent is released before the child is latched. The child may split in between,
which is exactly the case the move-right step repairs.

### Insert

1. Descend as in search, and push onto a stack the page each level was left from (after any
   move-right).
2. At the leaf, take an exclusive latch, and move right if needed; while moving right, latch the
   next page before releasing the current one, so that no split can slip in between.
3. If the entry fits, insert it and release the latch.
4. Otherwise split the leaf:
   1. Allocate the new page `Q`, and copy into it the upper half of the entries, `P`'s old high key
      and `P`'s old right link.
   2. Rewrite `P` with the lower half, the new high key (the separator) and a right link to `Q`.
      Until `P` changes, nothing can reach `Q`; once it changes, everything in `Q` is reachable
      through `P`. Latch `Q` exclusively before `P` is released, and log both pages in one WAL
      record, so that no reader and no recovery ever sees `P` pointing at an unwritten `Q`.
5. Keep `P` latched, pop the parent from the stack and latch it exclusively. The parent may have
   split since the descent, so move right from the remembered page until finding the page that
   holds the downlink to `P`. PostgreSQL matches the downlink by child page ID rather than by
   separator key.
6. Insert `(separator, Q)` immediately after that downlink, then release `P`. If the parent
   overflows, split it the same way and continue upwards.
7. If the stack is empty because the root split since the descent began, re-descend from the
   current root to the level above the one being fixed, moving right as needed.

### Root split

Split the old root like any other page, allocate a new root holding downlinks to both halves, and
then update the root ID in the meta page. A reader that read the old root ID still finds
everything, since the old root's right link leads to its new sibling.

### Delete

Lehman and Yao do not merge pages at all: an entry is removed from its leaf, and pages may become
sparse. PostgreSQL's approach, based on Lanin and Shasha, removes only pages that are completely
empty, in two stages:

1. **Half-dead:** remove the page's downlink from its parent, so its key range now belongs to its
   right sibling, and mark the page half-dead. Searches that still reach it move right.
2. **Unlink:** latch the left sibling, the page and the right sibling, in that order, point the
   siblings at each other, and mark the page deleted.
3. **Drain:** a deleted page keeps its right link and stays in place as a tombstone until no
   in-flight search can still hold its page ID. Only then can it be recycled.

The rightmost page of a level is never deleted, which keeps every move-right chain finite and the
root in place.

### Range scan

Descend to the start key as in search, then scan leaves through their right links. When moving to
the next leaf, use the right link that was read *while the current leaf was latched*. If the scan
re-reads the current right link later, after a split moved some of the entries it already returned
to a new page, it returns those entries twice.

Backward scans need a left link as well, which PostgreSQL adds; moving left is harder, since the
left sibling may have split since its link was read, and the scan has to move right again until it
finds the page whose right link points back to where it came from.

### Crash recovery

The two halves of a split are separate atomic actions, so a crash can leave a half split behind.
The tree is still correct, because the new page is reachable through its left sibling. PostgreSQL
logs each half as its own WAL record, flags the left page as having an incomplete split, and has
the next insertion that runs into the flag finish the job by inserting the missing downlink.

## Invariants

- Every page except the rightmost of its level has a high key, and every key on it is at most the
  high key.
- For every page `P` with right sibling `Q`, every key on `Q` is greater than `P`'s high key.
- Keys are unique within a level (with the leaf exception that a high key may equal the last key on
  its page). Lehman and Yao require this; non-unique user keys need a tiebreaker such as a row ID.
- Following right links from the leftmost page of any level visits every live page of that level,
  in key order, and ends at a page with no right link.
- Every live page is reachable from its left neighbour's right link, even if no downlink points to
  it.
- A search for any stored key, starting from the root and moving right whenever the key exceeds
  the high key, ends at the leaf that holds it.
- Latches are only ever acquired left to right within a level and bottom to top across levels while
  another latch is held.

## Implementation pitfalls

- **The wrong comparison in move-right.** Whether the check is `key > high_key` or
  `key >= high_key` depends on which side of the split the separator's equal keys go. Getting it
  wrong either misses keys or moves right forever. Derive it from the split code, and test keys
  equal to a high key.
- **Publishing the new page too early.** If `P`'s new right link becomes visible before `Q` is
  fully written, a reader can follow it to garbage. With exclusive latches on both pages this
  cannot happen in memory, but it can on disk: a crash after writing `P` and before writing `Q`
  leaves a dangling link unless both pages are covered by the same WAL record.
- **Deadlocks from latch order.** Acquiring a latch while holding one is only safe moving right or
  up. Never latch a page to the left or below while holding a latch, and make that rule something
  the code structure enforces, for example by having the descent function release the parent guard
  before returning the child guard.
- **Stale descent stacks.** The parent recorded on the way down may have split, or (with page
  deletion) been deleted. Always move right from the remembered page, and handle the empty-stack
  case after a root split.
- **Recycling pages too early.** A searcher may hold a page ID it read from a parent shortly before
  the page was deleted. Recycling the page then sends that searcher into an unrelated page. Wait
  until every operation that started before the deletion has finished.
- **Guards and lifetimes in Rust.** Latches are naturally modelled as `RwLock` read and write
  guards on buffer-pool frames, but guards borrow the lock, so returning one from a function and
  keeping it across a loop iteration takes some care. Owned guard types, or a frame type behind an
  `Arc` with an owned-guard API, keep the latch code readable. `std::sync::RwLock` also reports
  poisoning through a `Result`, which this crate's lints require handling without `unwrap`.
- **Latches held across I/O.** Reading a page from disk while holding a latch on its parent stalls
  every other thread that needs the parent. The search protocol above avoids this by releasing the
  parent first.

## Testing

- **Sequential correctness first.** With one thread, the tree must pass every
  [B+ tree](b-plus-tree.md) test: a model test against `BTreeMap` and a structural validator,
  extended to check high keys and right links on every level.
- **Deterministic half splits.** Add a test hook that pauses an insert between the two halves of a
  split. With the tree held in the half-split state, run searches and range scans for keys in the
  new page and check they find them by moving right.
- **Concurrent stress.** Run several writer threads inserting disjoint key ranges and reader
  threads looking up keys already known to be committed. Every committed key must always be found,
  and at the end the tree must equal the union of everything inserted. Use small pages so that
  splits are frequent.
- **Scan consistency.** Run a forward scan while other threads insert into the scanned range, and
  check the scan never returns a key twice and never skips a key that existed for the whole scan.
- **Schedule exploration.** Tools such as `loom` (exhaustive, for small cases) or `shuttle`
  (randomised) explore thread interleavings, but need the code to use their lock and atomic types,
  so put the latch type behind a small abstraction from the start.
- **Crash between the halves.** Stop after the left-level split record and before the parent
  update, recover, check that every key is still found, and check that the next insert completes
  the split.

## Further reading

- [Efficient Locking for Concurrent Operations on
  B-Trees](https://www.csd.uoc.gr/~hy460/pdf/p650-lehman.pdf), Lehman and Yao, 1981.
- [PostgreSQL nbtree
  README](https://github.com/postgres/postgres/blob/master/src/backend/access/nbtree/README). How a
  production B-link tree handles splits, deletion and recovery.
