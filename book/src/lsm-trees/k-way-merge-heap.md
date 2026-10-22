# K-way merge heap

A min-heap (or a tournament "loser" tree) holding the head of each of `k` sorted inputs, so the
smallest next item can be popped in `O(log k)`.

- **Used by:** LSM range scans and compaction, external merge sort in query engines.
- **Cost:** `O(n log k)` to merge `n` items from `k` inputs.
- **Notes:** when two inputs hold the same key, the newest one has to win, so ties are broken by
  input age.

## How it works

### Layout

The merge holds `k` input iterators, each already sorted, and a *binary min-heap* of their
current heads. A binary heap is a complete binary tree stored in an array: the children of
position `i` are at `2i + 1` and `2i + 2`, and every node is no larger than its children, so the
smallest element is at position 0.

<figure class="diagram">
{{#include figures/k-way-merge-heap-layout.svg}}
<figcaption>Three inputs, newest first, and the heap of their current heads (highlighted), ordered by <code>(key, rank)</code>. The tree is stored as the array <code>[(a,1), (b,0), (b,2)]</code>: the children of position 0 are at 1 and 2.</figcaption>
</figure>

Popping `(a, 1)` returns `a` and refills the heap with input 1's next key, `e`. The next pops
return `b` from rank 0 before `b` from rank 2, so the newer version of `b` comes first.

Each heap entry holds the head's key and the index of the input it came from. Ties between equal
keys are broken by *rank*, the input's age: rank 0 is the newest input, so for the same key the
newer version comes out first. With internal keys (see [memtable](memtable.md)), the sequence
number already makes every key unique and the rank is only a safety net.

### Initialise

1. For each input, read its first item. Push `(key, rank)` for every input that is not empty, and
   keep the item itself in a per-input slot, so the heap holds only small entries.
2. Heapify the array, or push the entries one by one.

### Next

1. The top of the heap names the input holding the smallest key; take that input's current item
   as the result.
2. Advance that input. If it has another item, replace the top entry with the new head and *sift
   it down*: swap it with its smaller child until neither child is smaller. If the input is
   exhausted, move the last array entry to the top, shrink the array, and sift down.
3. Return the result.

Replacing the top and sifting down once is cheaper than a separate pop and push, which sift
twice.

<figure class="diagram">
{{#include figures/k-way-merge-heap-next.svg}}
<figcaption>The first call to <code>next</code> on the heap above. It returns <code>a</code> from input 1, whose next key, <code>e</code>, replaces the top entry. <code>(e, 1)</code> is larger than its smaller child, <code>(b, 0)</code>, so the two swap; with no children below, the sift-down stops.</figcaption>
</figure>

### Deduplicate versions

Merging sources of an LSM tree yields several versions of the same user key in a row, newest
first. A read path or compaction wraps the merge to keep only what it needs:

1. Take the next item. If its user key equals the previous one returned, skip it: it is an older,
   shadowed version (unless a snapshot still needs it).
2. For a user-facing scan, also skip the key if its newest version is a tombstone.
3. A compaction keeps tombstones, except where the [LSM tree](lsm-tree.md) rules say it is safe to
   drop them.

<figure class="diagram">
{{#include figures/k-way-merge-heap-dedup.svg}}
<figcaption>The full merge of the three inputs above, each item marked with the rank of its input. Equal keys come out newest first, so deduplication keeps the first <code>b</code> and the first <code>e</code>, from rank 0, and skips the older versions behind them.</figcaption>
</figure>

### Seek

To start a range scan, seek every input to the first key at or after the start, then rebuild the
heap from the new heads.

### Variants

- **Tournament (loser) tree:** a complete binary tree with the `k` inputs at its leaves. Each
  internal node stores the *loser* of the match played there, and the overall winner is kept
  above the root. After the winner's input advances, its new head is replayed along the single
  path from its leaf to the root, against the loser stored at each node: exactly one comparison
  per level, `⌈log2 k⌉` in total. A heap's sift-down compares against both children at each
  level. Exhausted inputs are represented by a sentinel larger than every key.
- **Linear scan of heads:** with very few inputs, comparing all heads on every step is simpler
  and can be faster than maintaining a tree.
- **Reverse iteration:** a max-heap gives a descending merge. Switching direction in the middle of
  a scan requires repositioning every input other than the current one, which is why merging
  iterators that support both directions are noticeably more complex.

## Invariants

- Heap property: every entry is no larger than its children, comparing `(key, rank)`.
- The heap holds exactly one entry for each input that is not exhausted, and none for exhausted
  inputs.
- The output is non-decreasing by key; among equal keys, it is ordered from newest input to
  oldest.
- Before deduplication, the number of items returned equals the total number of items in the
  inputs.
- After deduplication, each user key appears at most once, with its newest version.

## Implementation pitfalls

- **`BinaryHeap` is a max-heap.** Wrap entries in `std::cmp::Reverse`, or implement `Ord` in
  reverse, and make sure that ordering includes the rank, so ties are not left to chance.
- **`Ord` disagreeing with `Eq`.** A derived `PartialEq` that compares fields the hand-written
  `Ord` ignores breaks `BinaryHeap`'s assumptions; implement all of them over the same `(key,
  rank)` pair.
- **Errors treated as exhaustion.** Inputs that read from disk yield `Result`s. Mapping an error
  to "no more items" silently drops part of the data; propagate it out of the merge.
- **Borrowing the heads.** Holding a reference to the current item while advancing its input does
  not borrow-check. Keep heads in a `Vec` indexed by input, let heap entries carry only the key and
  the input index, and return owned items (or clone the key) before advancing.
- **Forgetting to refill.** After taking an input's head, its next item must enter the heap, or
  that input is silently truncated.
- **Deduplicating on the wrong key.** Versions differ in their internal keys; compare *user* keys
  to decide what is shadowed.
- **Index arithmetic in a hand-written heap.** `2i + 1` can overflow in principle and the clippy
  configuration here denies it; use `checked_mul` and `checked_add`, and `swap` and `get` instead
  of indexing.
- **Unsorted input.** The merge does not check its inputs; one unsorted input produces unsorted
  output far from the bug. A `debug_assert` per input that each key is larger than the last
  catches it at the source.

## Testing

- Reference model: concatenate all inputs tagged with their rank, sort by `(key, rank)` with a
  stable sort, and compare with the merge's output.
- Newest wins: build a `BTreeMap` by applying the inputs from oldest to newest, and compare it with
  the deduplicated merge.
- Shapes: `k = 0`, `k = 1`, many inputs, empty inputs among non-empty ones, one long input with
  many short ones, and the same key in every input.
- Errors: an input that fails halfway must make the merge return that error.
- Equivalence: when you write a loser tree, run it against the heap on the same random inputs.

## Further reading

- Knuth, *The Art of Computer Programming*, vol. 3, section 5.4.1: multiway merging and tournament
  (loser) trees.
- [Mini-LSM](https://skyzh.github.io/mini-lsm/), Chi. The merge iterator chapter, including how to
  break ties between versions of the same key.
