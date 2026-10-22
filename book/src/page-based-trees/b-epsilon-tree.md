# Bε-tree

> Builds on [B+ tree](../page-based-trees/b-plus-tree.md).

A B-tree whose inner nodes also hold buffers of pending updates. Updates trickle down in batches, so
writes approach LSM speed while reads keep B-tree depth.

- **Used by:** TokuDB and TokuMX (as "fractal trees"), BetrFS.
- **Notes:** Brodal and Fagerberg, 2003.

## How it works

### Layout

Leaves are the same as in a [B+ tree](b-plus-tree.md): sorted `(key, value)` entries. An inner
node of size `B` (counted in entries) splits its space in two:

- about `B^ε` pivot keys and child pointers, so the fan-out is `B^ε` instead of `B`;
- a **buffer** of about `B - B^ε` pending *messages*, each addressed to one key in the node's
  subtree.

`ε` is a design-time constant between 0 and 1. At `ε = 1` there is no buffer and the tree is a
B-tree; at `ε = 0` it is a binary tree with large buffers (a buffered repository tree). `ε = 1/2`
is the usual middle ground: fan-out `√B`, which makes the tree about twice as tall as a B-tree with
the same node size.

<figure class="diagram">
{{#include figures/b-epsilon-tree-layout.svg}}
<figcaption>An inner node with two pivots and four buffered messages. Each message shows its key, its operation (<code>+1</code> is an upsert) and its sequence number.</figcaption>
</figure>

A **message** carries a key, a sequence number assigned when it entered the tree, and an
operation: put a value, delete (a tombstone), or *upsert*, which describes a change to the old
value (for example, "add 1") without reading it first. The buffer is the node's own on-disk
contents, not a separate memory structure; it is written and evicted with the node. In memory,
implementations keep each buffer sorted by key and then by sequence number, often in a balanced
tree.

Bε-tree nodes are much larger than B+ tree pages: the ;login: tutorial reports that TokuDB and
BetrFS use nodes of about 4 MB with a fan-out between 4 and 16.

### Insert, delete and upsert

1. Wrap the operation in a message with the next sequence number.
2. Add it to the root's buffer. If the root is a leaf, apply it directly instead.
3. If the root's buffer is now over capacity, flush, as below.

A delete is a tombstone message; it removes the key only when it reaches the leaf. An upsert
`(k, f, Δ)` is semantically `insert(k, f(query(k), Δ))`, but costs only an insert.

### Flush

1. Pick the child that the most buffered messages are addressed to.
2. Move all messages for that child, in sequence order, from this node's buffer into the child's.
   Moving a large batch is what spreads the cost of rewriting the child over many messages.
3. If the child is a leaf, apply the messages to it instead: a put replaces or inserts, a tombstone
   removes, an upsert computes the new value from the current one (or from "absent").
4. If the child's buffer is now over capacity, flush the child recursively.
5. If a leaf overflows, split it as in a B+ tree. If an inner node gets too many children, split it
   and divide its buffer between the two halves by key.

<figure class="diagram">
{{#include figures/b-epsilon-tree-flush.svg}}
<figcaption>Flushing the node above, with a capacity of four messages. Child 0 (here an inner node) is the destination of two messages, more than any other child, so both move down in sequence order, after the older message already in its buffer.</figcaption>
</figure>

A useful optimisation, used by TokuDB and BetrFS, is to send a batch whose messages all go to the
same leaf straight to that leaf, instead of writing it into every buffer on the way.

### Point query

1. Walk from the root to the leaf that would hold the key, as in a B+ tree.
2. On the way, collect every message for the key from each buffer.
3. Start from the leaf's value (or "absent"), and apply the collected messages oldest first: the
   ones from the deepest buffer first, and within one buffer in sequence order.
4. Return the result. The query does not write it back; the messages reach the leaf with a later
   flush.

All messages for a key sit on the root-to-leaf path for that key, so a query reads the same nodes
as a B-tree query on a tree of the same height.

### Range query

1. Descend to the leaves covering the range.
2. For every node visited, take the messages whose keys fall in the range from its buffer.
3. Merge the leaf entries with the messages from all the buffers above them, applying the messages
   per key oldest first, as in a [k-way merge](../lsm-trees/k-way-merge-heap.md).

### Costs

With `N` keys, node size `B` and `0 < ε ≤ 1`, the ;login: tutorial gives these I/O costs:

- insert: `O(log_B(N) / (ε · B^(1-ε)))`, amortised;
- point query: `O(log_B(N) / ε)`;
- range query of `k` results: `O(log_B(N) / ε + k / B)`.

At `ε = 1/2`, inserts are about `√B / 2` times cheaper than in a B-tree with the same node size,
and queries cost about twice as many I/Os. Larger nodes then win back the height.

## Invariants

- Leaf and pivot invariants are those of a [B+ tree](b-plus-tree.md).
- Every message in a node's buffer is addressed to a key within that node's key range, and would be
  routed to an existing child by the node's pivots.
- For a given key, every message in a node's buffer is newer (has a larger sequence number) than
  every message for the same key in any of its descendants' buffers. Flushes only move messages
  downwards, in order, so this follows if no flush skips a message.
- After a flush completes, no buffer is over capacity.
- The logical contents of the tree, computed by applying every buffered message to the leaves in
  the order above, equal the result of applying all operations in sequence order.

## Implementation pitfalls

- **Applying messages in the wrong order.** A put followed by a delete must not resurrect the key.
  Order by depth first (deeper is older) and by sequence number within a buffer, and never by key
  alone. A flush must move every message for the chosen child, not a subset, or a newer message can
  end up below an older one.
- **Upserts as closures.** A closure cannot be written to disk. Represent the upsert operation as an
  enum of supported operations (add, append, set a field…) that can be serialised and applied
  deterministically, and decide what applying it to an absent key means.
- **Splitting during a flush.** When a child splits while messages are being moved into it, the
  rest of the batch must be routed by the new pivots. Finish routing against the updated parent,
  rather than a copy of the old pivots.
- **Cascading flushes.** A flush can trigger flushes in the child and further down. Write it as a
  loop or bound the recursion by the tree height, and make sure every node it dirties is written.
- **Variable-size messages.** Capacity is really in bytes, not message count. A large value in a
  message can fill a buffer by itself; decide whether such messages bypass the buffer.
- **Range queries that only look at the path.** A range query must apply messages from every buffer
  above every leaf in the range, not only from the path to the first key.
- **Crash safety.** A flush rewrites a parent and a child together. Either log both to a
  [WAL](../storage-primitives/write-ahead-log.md) or write new copies and switch to them atomically,
  as a [copy-on-write B+ tree](copy-on-write-b-plus-tree.md) does.
- **Sequence-number overflow.** Sequence numbers only grow. Use `u64`, and `checked_add` so that the
  crate's overflow lints are satisfied and an overflow becomes an error rather than a wrap.

## Testing

- **Model test.** Run random puts, deletes, upserts, point queries and range queries against the
  tree and a `BTreeMap` that applies each operation immediately; they must always agree.
- **Tiny parameters.** Use a buffer capacity of a few messages and a fan-out of two or three, so
  flushes, cascades and splits happen in every test.
- **Same-key histories.** Generate many operations on a small set of keys, so that messages for the
  same key pile up in several buffers at once; this is where ordering bugs show.
- **Validator.** After each operation, walk the tree and check the message-placement and ordering
  invariants above.
- **Write amplification.** Count node reads and writes in a test byte store, and compare random
  inserts against the [B+ tree](b-plus-tree.md). This is not a correctness test, but it shows
  whether batching is working.

## Further reading

- [An Introduction to Bε-trees and
  Write-Optimization](https://www3.cs.stonybrook.edu/~bender/newpub/2015-BenderFaJa-login-wods.pdf),
  Bender et al., 2015. A short tutorial in ;login:.
- Brodal and Fagerberg, *Lower Bounds for External Memory Dictionaries*, SODA 2003.
