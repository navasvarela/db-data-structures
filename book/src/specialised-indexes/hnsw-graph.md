# HNSW graph

A hierarchical navigable small-world graph for approximate nearest-neighbour search over vectors.
Search descends from a sparse top layer to the dense bottom layer, greedily following edges towards
the query.

- **Used by:** pgvector, Lucene vector search, Qdrant, Weaviate.
- **Cost:** roughly logarithmic search; high memory use, since every vector keeps its edge list.
- **Notes:** Malkov and Yashunin, 2018. IVF (clustered inverted lists) is the main alternative.

## How it works

### Layout

The index stores `n` vectors and a graph over them, in layers numbered from 0 upwards:

- Every vector has a **level** `l`, drawn at random when it is inserted, and appears in layers
  `0` to `l`.
- In each layer it appears in, a vector has a list of **neighbours**: the IDs of other vectors in
  that layer. Edges are added in both directions, so the lists are usually close to symmetric, but
  pruning can make them one-way.
- One vector on the top layer is the **entry point**, where every search starts.

Most vectors have level 0, a fraction appear on layer 1 as well, a smaller fraction on layer 2, and
so on, so the upper layers are sparse graphs with long edges and layer 0 is a dense graph with
short ones. It is the same idea as a [skip list](../in-memory-indexes/skip-list.md), with
proximity graphs in place of linked lists.

<figure class="diagram">
{{#include figures/hnsw-graph-layers.svg}}
<figcaption>Nine vectors; <code>A</code> and <code>F</code> have level 2, <code>C</code> and <code>H</code> level 1. The highlighted path is a query for <code>q</code>, as described below: a greedy walk on each upper layer, dropping down from the vector it ends on, then a search of layer 0.</figcaption>
</figure>

The parameters, with the names the paper uses:

- `M`: number of neighbours chosen for a new vector on each layer. The paper suggests 5 to 48;
  memory use grows in proportion to it.
- `Mmax`, `Mmax0`: the most neighbours a vector may keep on layers above 0 and on layer 0. The paper
  uses `Mmax0 = 2 · M`.
- `mL`: normalisation factor for the level distribution. The paper recommends `mL = 1 / ln(M)`.
- `efConstruction`: size of the candidate list during insertion. Larger values build a better
  graph, more slowly.
- `ef`: size of the candidate list during a query. It is the main knob for recall against speed,
  and must be at least `k`.

### Search one layer

This is the core routine, used by both insert and query. Given a query vector `q`, a set of entry
vectors and a number `ef`, it returns about the `ef` nearest vectors to `q` it can find in one
layer:

1. Put the entry vectors in three sets: *visited*, *candidates* (a min-heap by distance to `q`),
   and *results* (a max-heap by distance, holding at most `ef` vectors).
2. Pop the nearest candidate `c`. If `c` is farther from `q` than the farthest result, stop: no
   unexplored candidate can improve the results.
3. For each neighbour `e` of `c` on this layer that has not been visited, mark it visited. If the
   results have fewer than `ef` vectors, or `e` is nearer than the farthest result, add `e` to both
   the candidates and the results, and if the results now exceed `ef`, drop the farthest.
4. Repeat from step 2 until the candidates are empty.

### Query (k nearest neighbours)

1. Start with the entry point, on the top layer.
2. On every layer above 0, run the layer search with `ef = 1`, a greedy walk, and use the single
   result as the entry for the layer below.
3. On layer 0, run the layer search with the query's `ef`.
4. Return the `k` nearest of the results.

### Insert

1. Draw the new vector's level: `l = floor(-ln(u) · mL)`, with `u` uniform in `(0, 1]`.
2. From the entry point, descend greedily (`ef = 1`) through the layers above `l`, as in a query.
3. On each layer from `min(l, top layer)` down to 0:
   1. Run the layer search with `ef = efConstruction` from the current entry vectors.
   2. Select `M` neighbours from the results (see below), and add edges in both directions.
   3. For each selected neighbour whose list now exceeds `Mmax` (or `Mmax0` on layer 0), shrink
      its list back to the limit by running the same neighbour selection on it.
   4. Use the layer's results as the entry vectors for the layer below.
4. If `l` is above the current top layer, the new vector becomes the entry point.

### Neighbour selection

The simple rule keeps the `M` nearest candidates. The paper's heuristic keeps edges in diverse
directions instead, which matters for clustered data, where the `M` nearest of a vector on the edge
of a cluster are all inside it, and the graph can split into disconnected clusters:

1. Go through the candidates from nearest to farthest.
2. Keep a candidate only if it is nearer to the new vector than to every neighbour already kept.
3. Stop at `M` kept neighbours. Optionally, fill any remaining slots with the nearest of the
   discarded candidates (the paper's `keepPrunedConnections`).

<figure class="diagram">
{{#include figures/hnsw-graph-neighbour-selection.svg}}
<figcaption>Choosing <code>M</code> = 3 neighbours for <code>v</code>. The heuristic keeps the two nearest, drops the third nearest because it is closer to a kept neighbour than to <code>v</code>, and keeps a link into the second cluster instead.</figcaption>
</figure>

### Delete

The paper does not define deletion. Common approaches are to mark the vector as deleted and filter
it out of results while still using it for navigation, then rebuild or repair the graph once enough
vectors are marked. Removing a vector outright needs its neighbours' lists repaired, or the graph
can lose connectivity.

### Variants

- **Distance functions:** Euclidean, inner product, or cosine (inner product on normalised
  vectors). The graph is built for one function and only answers queries for it.
- **Quantised vectors:** storing vectors in fewer bits makes distances cheaper and the index
  smaller; results are then often re-ranked with the full vectors.
- **Flat NSW:** a single layer (`mL = 0`), the authors' earlier design. Simpler, but the paper
  shows its search cost grows polylogarithmically rather than logarithmically with `n`.

## Invariants

- A vector with level `l` appears on exactly layers `0` to `l`, and every edge on layer `i` joins
  two vectors whose levels are at least `i`.
- No neighbour list exceeds `Mmax` on layers above 0, or `Mmax0` on layer 0, and none contains the
  vector itself or a duplicate.
- The entry point is a vector of the highest level present.
- Every neighbour ID refers to a vector in the index.
- A query returns at most `k` results, sorted by distance, with no duplicates.
- Connectivity is not guaranteed, but it should hold in practice: every vector on layer 0 should be
  reachable from the entry point. Check it in tests rather than assert it in production code.

## Implementation pitfalls

- **Heap directions.** The candidates are a min-heap and the results a max-heap. `BinaryHeap` is a
  max-heap, so one of them needs `Reverse`. Swapping them does not crash; it silently ruins recall.
- **Floating-point ordering.** Distances are `f32` or `f64`, which are not `Ord`, and `NaN`
  propagates silently. Reject vectors with `NaN` or infinite components at insert, and order
  distances with `total_cmp`.
- **The level formula at `u = 0`.** `ln(0)` is minus infinity, which makes the level infinite. Draw
  `u` from `(0, 1]`, and cap the level. Converting the float to an integer also trips clippy's
  pedantic cast lints in this crate; clamp it to a valid range first and convert explicitly.
- **Forgetting to prune neighbours.** Adding the reverse edge without shrinking the neighbour's
  list lets it grow without bound, and lists that are pruned with the simple rule rather than the
  heuristic lose the long edges that keep the graph navigable.
- **The visited set.** Allocating a fresh `HashSet` per query is slow. A common trick is a vector of
  visit marks indexed by vector ID, with a query counter as the mark, so it never needs clearing.
- **Graphs and ownership.** The graph is full of cycles, so nodes cannot own each other. Store
  vectors in one array and refer to them by a `u32` ID everywhere; neighbour lists are lists of IDs.
  Indexing that array with `v[i]` is denied in this crate, so go through `get` and treat a missing
  ID as corruption.
- **Concurrent inserts.** Inserting changes the neighbour lists of existing vectors. A per-vector
  lock around each list, and a separate lock for the entry point, is the usual approach; take
  only one list lock at a time to avoid deadlocks.
- **Non-determinism.** Random levels and thread scheduling make the graph different on every build,
  and so the exact results. Seed the random generator in tests.
- **Small indexes.** The empty index (no entry point), the first insert, and `k` larger than the
  number of vectors all need handling.

## Testing

- **Recall against exact search.** Compute the true `k` nearest neighbours of each query by a
  brute-force scan, and measure recall, the fraction of them the index returns. Assert that recall
  is above a threshold you measured, for example on a few thousand random vectors, rather than a
  number from a paper. Test clustered data too, where a weak neighbour selection shows up.
- **Exactness for large `ef`.** With `ef` at least the number of vectors, the layer-0 search keeps
  every vector it reaches, so on a connected graph the results must equal brute force.
- **Structural validator.** After building, check every invariant above, and count the vectors not
  reachable from the entry point on layer 0.
- **Recall as `ef` grows.** Recall should not get noticeably worse as `ef` increases; a drop points
  at a heap or stopping-condition bug.
- **Deletes.** Mark random vectors deleted and check they never appear in results, while recall
  over the remaining vectors stays close to that of an index built without them.
- **Edge cases.** Duplicate vectors, all-identical vectors, a single vector, and `k = 0`.

## Further reading

- [Efficient and robust approximate nearest neighbor search using Hierarchical Navigable Small World
  graphs](https://arxiv.org/abs/1603.09320), Malkov and Yashunin, 2018.
