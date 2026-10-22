# Consistent hashing ring

Places nodes and keys on a hash ring, with each key owned by the next node clockwise. Adding or
removing a node moves only about `1/n` of the keys. Virtual nodes even out the load.

- **Used by:** Amazon Dynamo, Cassandra, Riak.
- **Notes:** Karger et al., 1997. Rendezvous (highest random weight) hashing is an alternative.

## How it works

### Layout

The *ring* is the range of a hash function, for example all `u64` values, with the largest value
wrapping round to 0. Each node is placed on the ring at one or more positions called *tokens*. A
key is hashed onto the ring and belongs to the first token at or after its hash, moving clockwise;
the node that owns that token owns the key. Each token therefore owns the arc between the previous
token (exclusive) and itself (inclusive).

<figure class="diagram">
{{#include figures/consistent-hashing-ring-layout.svg}}
<figcaption>Three nodes with two tokens each, and the ring stored as a sorted map. Key <code>k</code> hashes between B1 and C1, so it belongs to C1's arc, <code>(B1, C1]</code>, and node C owns it. A1's arc runs from C2 through 0 to A1.</figcaption>
</figure>

In memory, the ring is a sorted map from token to node, such as a `BTreeMap<u64, NodeId>`. A
*virtual node* (vnode) is one of a physical node's several tokens: with `T` tokens per node, each
node owns `T` scattered arcs instead of one.

### Lookup

1. Hash the key to a ring position `h`.
2. Find the first token `>= h` in the sorted map (`range(h..).next()`).
3. If there is none, wrap round to the smallest token.
4. Return that token's node.

### Replica placement

With replication factor `N`, a key is stored on `N` nodes. Dynamo calls this list the key's
*preference list*:

1. Find the key's owning token as above.
2. Walk clockwise from it, collecting each token's node.
3. Skip a node already in the list. With vnodes, consecutive tokens often belong to the same
   physical node, and two replicas on one machine are not two replicas. The Dynamo paper builds the
   preference list in exactly this way.
4. Stop when `N` distinct nodes have been collected, or when the walk returns to the start.

### Adding a node

1. Choose the new node's tokens.
2. For each new token `t`, the arc between the previous token and `t` used to belong to `t`'s
   clockwise successor. That arc, and only that arc, moves to the new node.
3. Stream the keys in the moving arcs from their old owners to the new node.
4. Switch ownership once the data has arrived. Until then, reads keep going to the old owner, or
   writes go to both.

With `n` nodes and evenly spread tokens, the new node takes about `1/(n + 1)` of the keys, and every
moved key moves to the new node. No key moves between two old nodes.

<figure class="diagram">
{{#include figures/consistent-hashing-ring-join.svg}}
<figcaption>Node D joins with tokens D1 and D2. D1 takes over part of C1's arc and D2 part of B2's, so <code>k2</code> moves from C to D and <code>k4</code> from B to D. With its two tokens in different places, D takes load from two different nodes.</figcaption>
</figure>

### Removing a node

1. Remove the node's tokens from the map.
2. Each removed token's arc now belongs to the next token clockwise.
3. If the node left cleanly, stream its keys to those successors. If it failed, the successors
   rebuild the data from the other replicas.

With vnodes, the removed node's arcs are spread over many successors, so its load is shared out
among many nodes instead of landing on one neighbour. The Dynamo paper lists this, together with the
reverse when a node joins and the ability to give stronger machines more tokens, as the advantages
of virtual nodes.

### Variants

- **Fixed partitions.** Dynamo's later scheme divides the hash space into `Q` equal partitions,
  fixed once, and gives each of `S` nodes `Q/S` of them. Partitioning and placement are then
  separate decisions: a partition moves between nodes as a unit, and the paper reports better load
  balance and much smaller membership metadata than with random tokens.
- **Rendezvous (highest random weight) hashing.** For each key, compute `hash(key, node)` for every
  node and pick the highest. It needs no ring and no vnodes, and moves the minimum number of keys,
  but a lookup costs `O(n)` hashes.
- **Jump consistent hash.** Lamping and Veach's algorithm maps a key to one of `n` buckets with no
  memory at all and near-perfect balance, but buckets are numbered `0..n` and only the last one can
  be removed. It suits sharding where shards are added and removed at the end, not arbitrary node
  failures.

## Invariants

- Every ring position maps to exactly one token, and tokens are unique.
- Lookup is deterministic: the same key and the same membership always give the same node, on
  every process.
- Adding a node only moves keys to that node; removing a node only moves keys that were on it.
- A preference list contains `min(N, number of nodes)` distinct physical nodes.
- The arcs of all tokens together cover the whole ring exactly once.

## Implementation pitfalls

- **Unstable hashing.** Every client and every node must compute the same position for the same
  key, across processes, machines and releases. Rust's `DefaultHasher` and `RandomState` are seeded
  per process and their algorithm may change, so they cannot be used here. Use a hash with a
  published specification and a fixed seed, and hash a canonical byte encoding of the key, not its
  in-memory layout.
- **Wrap-around.** Forgetting step 3 of the lookup sends keys that hash above the largest token
  nowhere. Arc lengths need wrapping arithmetic too: the arc of the smallest token runs from the
  largest token round through 0, so compute it with `wrapping_sub`, since clippy denies plain
  overflowing subtraction.
- **Vnode placement.** Tokens are often derived as `hash(node_id, i)` for `i` in `0..T`. If two
  nodes can produce the same token, the map silently drops one; detect collisions and resolve them
  deterministically. Too few tokens per node leaves large imbalances: the arcs of randomly placed
  tokens vary widely in length, and only many tokens per node average that out.
- **Replicas on the same failure domain.** Distinct nodes are not enough if two of them share a
  rack or zone. To survive the loss of a rack, step 3 of replica placement has to skip nodes until
  the list spans distinct racks or zones as well, where enough of them exist.
- **Membership disagreement.** During a change, nodes that have not yet seen the new membership
  route keys to the old owner. Version the membership, and have a node that receives a key it no
  longer owns redirect or forward, rather than store, it.
- **Uniformity assumptions.** "About `1/n` of the keys moves" assumes keys hash uniformly and the
  load is spread evenly across keys. A few very hot keys stay on one node however many tokens it
  has.
- **Merkle trees over changing arcs.** Every token change alters the arcs that anti-entropy
  compares, forcing hash trees to be rebuilt. The Dynamo paper reports this as one of the problems
  that led to fixed partitions (see [Merkle tree](merkle-tree.md)).

## Testing

- **Reference model.** A lookup that scans the sorted token list linearly for the first token
  `>= h`, wrapping to the first. Compare with the map-based lookup for many random keys and rings.
- **Movement.** Build a ring, record the owner of 100,000 keys, add a node, and assert every key
  that changed owner now belongs to the new node, and that the fraction moved is close to
  `1/(n + 1)`. Do the same for removal.
- **Balance.** For a range of token counts, measure the largest node's key count divided by the
  average, and check it shrinks as tokens per node grow.
- **Determinism.** Assert that hard-coded keys map to hard-coded ring positions, so a change of hash
  function or encoding fails a test instead of reshuffling a cluster.
- **Edge cases:** an empty ring (lookup must return an error, not panic), a ring with one node, a
  key that hashes exactly onto a token, positions 0 and `u64::MAX`, and `N` larger than the node
  count.

## Further reading

- [Consistent Hashing and Random Trees](https://doi.org/10.1145/258533.258660), Karger et al., 1997.
- [Dynamo: Amazon's Highly Available Key-value
  Store](https://www.allthingsdistributed.com/files/amazon-dynamo-sosp2007.pdf), DeCandia et al.,
  2007. Section 4.2 covers partitioning with virtual nodes.
- [A Fast, Minimal Memory, Consistent Hash Algorithm](https://arxiv.org/abs/1406.2294), Lamping and
  Veach, 2014. Jump consistent hashing.
