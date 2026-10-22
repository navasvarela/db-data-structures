# Merkle tree

A tree of hashes in which each inner node hashes its children. Two replicas compare root hashes and
descend only into differing subtrees, finding divergent ranges without exchanging all data.

- **Used by:** Cassandra and Dynamo anti-entropy repair, Git, certificate transparency logs.
- **Notes:** Merkle, 1987.

## How it works

### Layout

For replica repair, the tree is built over a key range that both replicas agree on, such as one
partition of a [consistent hashing ring](consistent-hashing-ring.md). The range is split into `2^d`
equal sub-ranges, one per leaf. A leaf's hash summarises every key-value pair whose key falls in its
sub-range; each inner node's hash is the hash of its two children's hashes concatenated.

<figure class="diagram">
{{#include figures/merkle-tree-layout.svg}}
<figcaption>A tree of depth <code>d = 2</code> over the range <code>[0, 400)</code>. Each leaf hashes the pairs in its sub-range; each inner node hashes its two children. The small numbers are the nodes' positions when the tree is stored as an array.</figcaption>
</figure>

The leading `0` and `1` bytes mark leaf and inner hashes as different kinds of input; the pitfalls
explain why. Stored as an array, the tree takes `2^(d+1) - 1` hashes, in the same implicit layout as
a binary heap: the children of node `i` are `2i + 1` and `2i + 2`.

### Build

1. Iterate the replica's keys in the range in key order. This is one scan of the data.
2. For each key, find its leaf from its position in the range, and feed the key, its version and its
   value into that leaf's hash state.
3. Finish every leaf hash, including the leaves that received no keys; an empty leaf still gets the
   hash of an empty input.
4. Compute inner nodes level by level from the bottom, up to the root.

### Compare and repair

Two replicas, `A` and `B`, compare trees over the same range:

1. `A` sends its root hash. If it equals `B`'s, the ranges hold the same data and the exchange ends.
2. Otherwise, exchange the two children's hashes, and recurse only into the children that differ.
3. At a differing leaf, exchange the keys in that sub-range, with their versions or hashes, and
   work out which keys differ.
4. Reconcile each differing key with the store's normal rule, for example "newest version wins",
   and send the winning value to the replica that lacks it.

<figure class="diagram">
{{#include figures/merkle-tree-compare.svg}}
<figcaption>Two replicas that differ only in a key between 300 and 400. The comparison descends one level per round trip, and only the keys of one leaf are exchanged; the subtree under <code>h01</code> is never looked at.</figcaption>
</figure>

With few differences, the exchange costs about `d` hashes per differing leaf, instead of the whole
data set. The Dynamo paper also notes that comparing trees reduces the disk reads of anti-entropy,
since matching subtrees need no data access at all.

### Update

A tree can be rebuilt periodically from a scan, or kept current:

1. On every write, recompute the hash of the affected leaf.
2. Recompute each ancestor up to the root, `d` hashes in all.

Recomputing a leaf from scratch means rereading every key in it. One alternative is to make the
leaf hash the XOR (or a sum modulo `2^64`) of a hash of each key-value pair, so a write is "remove
the old pair's hash, add the new one". It is fast and independent of order, but weaker than
hashing the whole leaf: XOR is linear, so two identical pair hashes cancel, and anyone choosing the
data can construct different sets of pairs with the same combination. That is acceptable between
trusted replicas, not for detecting tampering.

### Variants

- **Hash trees over an append-only log.** Certificate Transparency (RFC 6962) builds a tree over a
  list that only grows, splitting `n` entries at the largest power of two below `n`. It supports
  short proofs that an entry is in the log, and that a newer log extends an older one.
- **Higher fanout.** Each inner node hashes more children. The tree is shallower, so fewer round
  trips are needed, but each differing node sends more child hashes.
- **Content-defined boundaries.** Instead of fixed sub-ranges, some trees split by key content so
  that inserting one key changes few nodes. These are more complex and not needed for repair over
  fixed ranges.

## Invariants

- Every inner node's hash equals the hash of its children's hashes, in order, with the inner-node
  prefix.
- A leaf's hash depends only on the set of key-value pairs in its sub-range, never on insertion
  order or on how the data is laid out on disk.
- Two replicas compare only trees with identical range boundaries, depth and hash function.
- If two trees' roots are equal, the data is equal, up to the probability of a hash collision.
- After repair of all differing keys, both trees have the same root.

## Implementation pitfalls

- **Comparing trees over changing ranges.** If a range's boundaries move, for example when a ring
  gains a node, existing trees describe ranges that no longer exist and must be rebuilt. The Dynamo
  paper names this as the disadvantage of its first scheme and avoided it by switching to fixed,
  equal-sized partitions.
- **Data changing during the build.** Writes that arrive while the trees are being built or compared
  show up as differences that are not real, or hide real ones until the next round. Build from a
  consistent snapshot of the range when the store offers one, and accept that repair converges over
  several rounds rather than in one.
- **Deletes.** A missing key and a deleted key look the same unless deletes are kept as tombstones
  and hashed like values. Without them, repair copies the deleted value back from the replica that
  never saw the delete.
- **Unstable hashing and encoding.** Both replicas must hash identical bytes with an identical
  function, possibly on different machines and software versions. Rust's `DefaultHasher` and
  `RandomState` change per process, so they cannot be used. Define a canonical encoding for keys,
  versions and values, including length prefixes so that `("ab", "c")` and `("a", "bc")` do not
  encode to the same bytes.
- **Leaf and inner hashes that can be confused.** Without a prefix that tells leaf input from inner
  input, a leaf whose content happens to equal the concatenation of two child hashes has the same
  hash as that inner node. RFC 6962 prefixes leaf input with `0x00` and inner input with `0x01` for
  this reason. For repair between trusted replicas this matters less than for public logs, but the
  prefix costs nothing.
- **Odd numbers of children.** Trees over a list of arbitrary length must decide what to do with an
  unpaired node. Duplicating it makes the lists `[a, b, c]` and `[a, b, c, c]` hash the same.
  Promote the unpaired node unchanged, or split at a power of two as RFC 6962 does. A fixed `2^d`
  leaf layout avoids the question.
- **Skewed leaves.** With fixed sub-ranges and unevenly spread keys, one leaf can cover most of the
  data, and a single difference there means streaming most of the range. Choose `d` from the key
  count, or split leaves by key count instead of by range.
- **Choosing the depth.** Each extra level doubles the tree's size and halves the data streamed per
  differing leaf. Repair over a slow link wants deeper trees; frequent rebuilds want shallower ones.

## Testing

- **Reference model.** Compare two replicas, held as `BTreeMap`s, by brute force: the set of keys
  whose value or version differs, or that exist on only one side. The tree's comparison must report
  every leaf that contains one of those keys, and only those leaves.
- **Order independence.** Insert the same pairs in several random orders and assert the root is the
  same every time.
- **Repair convergence.** Generate two replicas with random differences (missing keys, different
  values, tombstones on one side), run compare and repair, and assert both maps and both roots are
  equal afterwards.
- **Round trips.** Count the hashes exchanged and assert the count grows with the number of
  differing leaves, not with the data size.
- **Edge cases:** an empty range, a single key, all keys in one leaf, two trees of different depth
  (comparison must refuse, not give an answer), and keys exactly on sub-range boundaries.

## Further reading

- [A Digital Signature Based on a Conventional Encryption
  Function](https://doi.org/10.1007/3-540-48184-2_32), Merkle, 1987.
- [Dynamo: Amazon's Highly Available Key-value
  Store](https://www.allthingsdistributed.com/files/amazon-dynamo-sosp2007.pdf), DeCandia et al.,
  2007. Section 4.7 uses Merkle trees for replica synchronisation.
