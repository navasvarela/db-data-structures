# Bw-tree

A latch-free B-tree. Pages are addressed through a mapping table, and updates are prepended as delta
records with a compare-and-swap on the mapping entry instead of being written in place.

- **Used by:** Microsoft SQL Server Hekaton range indexes, Azure Cosmos DB.
- **Notes:** Levandoski et al., 2013. Hard to get right; worth tackling after the B-link tree.

## How it works

### Layout

The Bw-tree is a [B-link tree](b-link-tree.md) whose nodes are never modified after they are
published. Three pieces make that work.

- **Mapping table.** Every node has a logical page ID (PID), an index into an array of pointers.
  All links between nodes, the child pointers in inner nodes and the right-sibling links, are PIDs,
  never memory addresses. To find a node, a thread reads its slot in the mapping table.
- **Base node.** An immutable, sorted array: `(key, value)` pairs in a leaf, `(separator, child
  PID)` pairs in an inner node. Like a B-link page, it also records a low key, a high key and the
  PID of its right sibling.
- **Delta records.** An immutable record describing one change, such as "insert key 7" or
  "delete key 9", with a physical pointer to the previous state of the node. The mapping-table slot
  points at the newest delta; following the chain ends at the base node.

<figure class="diagram">
{{#include figures/bw-tree-layout.svg}}
<figcaption>Grey cells hold PIDs, never addresses: the inner node's children are PIDs 1 and 2, and <code>→2</code> is the leaf's right sibling. Logically, PID 1 holds 20, 30 and 40.</figcaption>
</figure>

Because every reference to a node goes through its slot, a single compare-and-swap (CAS) on that
slot replaces the node's whole state for every thread at once. A CAS writes a new value to a memory
location only if it still holds an expected old value, atomically.

### Update (insert, delete, modify)

1. Descend to the leaf, as below, and read the leaf's current pointer `p` from its slot.
2. Allocate a delta record describing the change, pointing to `p`.
3. CAS the slot from `p` to the new delta.
4. If the CAS fails, another thread changed the node first. Discard the delta and retry. The
   OpenBw-Tree authors restart the whole operation from the root, which is the simplest choice.

<figure class="diagram">
{{#include figures/bw-tree-update.svg}}
<figcaption>Inserting 30 into PID 1. The delta is private until the CAS succeeds; if another thread moved the slot away from <code>p</code> first, the CAS fails and nothing has changed.</figcaption>
</figure>

The original paper's leaf deltas are insert, modify and delete, and each carries the log sequence
number of the transaction that made it.

### Search

1. Start at the root's PID. Read its slot.
2. Walk the node's delta chain from the newest record. In a leaf, the first delta for the search
   key decides the answer: an insert or modify returns its value, a delete returns "not found".
3. If no delta mentions the key, binary-search the base node.
4. Before trusting a node, check its key range, as in a B-link tree: if the key is at or beyond the
   node's high key, follow the right-sibling PID.
5. In an inner node, choose the child PID the same way (deltas first, then the base), and repeat.

### Consolidation

Long delta chains make every search slower, so a thread that notices a chain longer than a
threshold consolidates it after finishing its own operation:

1. Read the slot's current pointer `p`.
2. Build a new base node by replaying the chain onto a copy of the old base node: apply each delta,
   drop deleted keys, and sort.
3. CAS the slot from `p` to the new base.
4. On success, hand the old chain and base to the garbage collector. On failure, discard the new
   node and do not retry; a later thread will consolidate.

<figure class="diagram">
{{#include figures/bw-tree-consolidation.svg}}
<figcaption>Consolidating PID 1 from the first figure. Threads that loaded the old head before the CAS can still be walking the old chain, so it is only retired, not freed.</figcaption>
</figure>

The original authors reported a chain length of eight as a good threshold. The OpenBw-Tree paper
found that separate thresholds for inner and leaf nodes worked better.

### Split

A split changes three nodes (the full node `P`, its new sibling `Q`, and the parent `O`), which
cannot be done with one CAS. It is done as a B-link half split followed by a parent update, each
step a single CAS:

1. **Child split.** Choose a separator `Kp` in `P`. Build a base node `Q` with `P`'s entries from
   `Kp` upwards, `P`'s old high key and `P`'s old right-sibling PID. Allocate a new PID for `Q` and
   store `Q` in its slot; no CAS is needed, since no other thread can see this PID yet.
2. **Split delta.** CAS a *split delta* onto `P` that records `Kp` and `Q`'s PID. From now on, a
   search in `P` for a key at or beyond `Kp` follows the split delta to `Q`. The tree is valid even
   though `O` does not know about `Q` yet.
3. **Index entry delta.** CAS an *index entry delta* onto the parent `O`, recording `Kp`, `Q`'s PID
   and the separator after it. Searches now go straight to `Q`.

<figure class="diagram">
{{#include figures/bw-tree-split.svg}}
<figcaption>Splitting PID 1 (<code>P</code>, base keys 10 to 40) at <code>Kp</code> = 30. The split delta shadows 30 and 40 in the base node (grey), and the index entry delta tells the parent that keys from 30 up to 50 are in PID 3.</figcaption>
</figure>

If the CAS in step 2 fails, `Q` was never visible to another thread, so it can be discarded and
its PID reused at once. If step 3 has not happened yet, the tree is still correct, only slower:
searches for `Q`'s keys go through `P` and its split delta.

### Merge

A merge needs one more step than a split. To remove `R` into its left sibling `L`:

1. **Remove node delta.** CAS a remove-node delta onto `R`. Threads that reach `R` now go to `L`
   instead.
2. **Node merge delta.** CAS a merge delta onto `L` that points physically at `R`'s old state and
   records the separator between them. `L`'s chain is now a small tree: searches below the
   separator continue into `L`'s own state, the rest into `R`'s.
3. **Index term delete delta.** CAS a delta onto the parent that removes `R`'s separator and
   extends `L`'s range.
4. Once no thread can reach it, free `R`'s PID.

### Helping along

A thread that runs into a half-finished structure modification (a split delta whose index entry
is missing from the parent, a remove-node delta, a merge delta) completes that step before
continuing its own operation. No thread ever waits for another, and an operation abandoned halfway,
for example because its thread was descheduled, does not stall the tree.

### Garbage collection

After a successful consolidation or merge, the old chain may still be being read by threads that
loaded the slot earlier. The original design uses *epochs*: a thread enters the current epoch before
an operation and leaves it after; memory retired during epoch `E` is freed once every thread that
was in epoch `E` or earlier has left. PIDs are protected the same way, so a PID is not reused while
a thread may still hold it.

### Range scan

The original paper builds a vector of the records on the current leaf and serves "next record" from
it, checking before each record whether the leaf has changed. The OpenBw-Tree iterator keeps a
private, consolidated copy of one leaf, and when it runs out, re-descends from the root with the
copy's high key to find the next leaf, instead of following a sibling pointer that may be stale.

### Variants

- **Original Bw-tree with log-structured storage.** The Microsoft design sits on a log-structured
  page store: the mapping table can point at a flash offset as well as a memory address, and a
  flush writes only the deltas since the page was last flushed. This page covers the in-memory tree
  alone.
- **OpenBw-Tree.** Wang et al. fill in what the paper leaves out: non-unique keys, iterators, a
  growable mapping table, faster consolidation, and an *abort delta* that prevents a parent split
  from racing with a child merge. In their measurements, it was still slower than the lock-based
  in-memory indexes they compared it with.

## Invariants

- Base nodes and delta records are never modified after they become reachable.
- Every live PID's slot points to a chain that ends in exactly one base node, with no cycles.
- Each logical node covers a key range `[low key, high key)`, and the ranges of the nodes on one
  level, in right-sibling order, are contiguous and disjoint.
- Every key in the tree is reachable from the root by the search procedure, at every moment,
  including during a split or merge.
- For a given key in a leaf, the newest delta mentioning it determines its value; later deltas in
  the chain (older ones) and the base node are shadowed.
- No memory and no PID is reused while a thread that started before its retirement is still
  running.

## Implementation pitfalls

- **`unsafe` and atomic pointers.** The paper's design is raw pointers with CAS and epoch-based
  reclamation. This crate forbids `unsafe`, and dereferencing the pointer loaded from
  `crossbeam-epoch` is an `unsafe` operation, so it cannot be used directly. A safe alternative is
  to store each slot as an `arc_swap::ArcSwap` of an `Arc` node: `compare_and_swap` is safe, and
  reference counting replaces epochs, at the cost of reference-count traffic on every read.
- **ABA.** A CAS on a raw address succeeds if the address was freed and reused for a new node
  between the read and the CAS, even though the node changed. Epochs prevent it by delaying reuse;
  with `Arc`, the thread doing the CAS holds a reference to the old node, so its address cannot be
  reused.
- **Dropping long chains.** A delta chain built from `Arc` links is dropped recursively by default,
  one stack frame per record. Long chains, especially after merge deltas join two chains, can
  overflow the stack. Implement `Drop` iteratively, or keep the consolidation threshold small.
- **Forgetting to help along.** Searches stay correct past an unfinished split, which makes it
  tempting to ignore it. But a thread that starts its own split or merge on a node whose previous
  modification is unfinished can interleave the two into a state neither protocol expects. As both
  papers require, finish any unfinished modification on a node before operating on it.
- **Concurrent split and merge.** The OpenBw-Tree paper describes a parent splitting while one of
  its children is being merged, which leaves a stale separator behind. Their fix is an abort delta
  that blocks other changes to the parent while a merge finishes. Decide early whether to support
  merges at all; a first version can skip them.
- **Consolidating across a split.** A node with a split delta must be consolidated into a base node
  that keeps only the keys below the separator, with the new high key and sibling. Replaying the
  whole chain without honouring the split resurrects keys that now belong to `Q`.
- **Non-unique keys.** Stopping at the first delta for a key is only correct if keys are unique.
  For duplicates, the OpenBw-Tree tracks the sets of values seen as present and as deleted while
  walking the chain.
- **Mapping table growth.** A fixed-size array of slots runs out. Allocate it in chunks that are
  never moved, so a slot's address is stable, and free PIDs only after the reclamation scheme
  allows it.

## Testing

- **Single-threaded model test first.** Compare against `BTreeMap` with random inserts, deletes,
  lookups and scans, with tiny thresholds (a chain length of two, nodes of four entries) so that
  consolidation and splits happen constantly.
- **Structural validator.** With no concurrent writers, walk every level and check key ranges, chain
  termination and reachability of every key; run it after every operation in the model test.
- **Forced interleavings.** Add test hooks that stop a split after step 1 or step 2 and a merge
  after each of its steps, then run searches, inserts and scans against the half-finished state and
  check that they succeed and that they help finish it.
- **Concurrent stress.** Many threads inserting disjoint key ranges, plus readers checking that keys
  known to be inserted are always found. At the end, compare the full contents with the union of
  inserts.
- **Linearisability checks.** Record a history of concurrent operations with timestamps and check it
  against a sequential model, for small histories. Tools such as `loom` or `shuttle` can explore
  interleavings, but only if the CAS goes through their atomic types, so put it behind a small
  trait.
- **Memory.** Run the stress test under a leak checker or with allocation counters to confirm that
  retired chains are actually freed.

## Further reading

- [The Bw-Tree: A B-tree for New Hardware
  Platforms](https://faculty.cc.gatech.edu/~jarulraj/courses/4420-s19/papers/08-oltpindexes1/bwtree-icde2013.pdf),
  Levandoski, Lomet and Sengupta, 2013.
- [Building a Bw-Tree Takes More Than Just Buzz
  Words](https://www.cs.cmu.edu/~huanche1/publications/open_bwtree.pdf), Wang et al., 2018. What the
  original paper leaves out, from an implementation that had to fill the gaps.
