# Data cube and star-tree index

> Builds on [partitioned hash table](../query-execution/partitioned-hash-table.md).

Aggregates precomputed for combinations of dimensions, so a `GROUP BY` over them reads a few
pre-aggregated rows instead of the raw data. A full cube over `d` dimensions has `2^d` groupings; a
star-tree materialises only some of them, as a tree that splits on one dimension per level and adds
a "star" child aggregating over all values of that dimension.

- **Used by:** Apache Pinot's star-tree index, Apache Kylin, Apache Druid's ingestion-time rollup,
  ClickHouse `AggregatingMergeTree` and projections, MOLAP servers such as Essbase.
- **Cost:** storage grows with the number of groupings kept. Only mergeable aggregates work: sums,
  counts, minimums and maximums directly, averages through sum and count, distinct counts through a
  [HyperLogLog](../sketches/hyperloglog.md) or [theta sketch](../sketches/theta-sketch.md).
- **Notes:** Gray et al., 1997 (the data cube); Harinarayan, Rajaraman and Ullman, 1996 (choosing
  which groupings to materialise).

## How it works

### The cube and its lattice

Take a fact table with dimensions `country`, `browser` and `locale` and a metric `impressions`. A
grouping (or cuboid) is the result of `GROUP BY` some subset of the dimensions. The full cube is
all `2^3 = 8` groupings, which form a lattice: a grouping can be computed from any grouping above
it that has a superset of its dimensions.

<figure class="diagram">
{{#include figures/data-cube-and-star-tree-index-lattice.svg}}
<figcaption>The lattice of the full cube over three dimensions, from one row per combination at the top to the grand total at the bottom. Each line joins a grouping to one with a dimension fewer; <code>(country)</code> can be rolled up from either grouping directly above it (blue).</figcaption>
</figure>

Gray et al. represent the cube as one relation by adding a value `ALL` to each dimension, meaning
"aggregated over this dimension". If dimension `i` has `Cᵢ` distinct values, the cube has at most
`∏ (Cᵢ + 1)` rows. With a few high-cardinality dimensions this explodes, which is why systems
materialise only part of it.

### Which aggregates work

A grouping can be rolled up into a coarser one only if the aggregate can be merged. Gray et al.
sort aggregates into three classes:

- **Distributive:** the aggregate of the parts is the aggregate of the whole: `SUM`, `COUNT` (merged
  by summing), `MIN`, `MAX`.
- **Algebraic:** a fixed-size summary merges: `AVG` stored as `(sum, count)`, variance as
  `(count, sum, sum of squares)`.
- **Holistic:** no fixed-size summary exists: exact median, exact distinct count, mode. These cannot
  be pre-aggregated exactly; mergeable sketches such as [HyperLogLog](../sketches/hyperloglog.md),
  [theta sketches](../sketches/theta-sketch.md) and [t-digest](../sketches/t-digest.md) give
  approximate answers that can.

### Choosing groupings to materialise

Harinarayan, Rajaraman and Ullman model the problem on the lattice. Each grouping's cost is its
row count, a query is answered from the smallest materialised grouping above it, and the benefit
of materialising a grouping is how much it reduces the cost of the groupings below it. Their greedy
algorithm starts from the finest grouping and repeatedly adds the grouping with the largest
benefit until it has chosen `k` of them. Its total benefit is guaranteed to be at least `1 - 1/e`,
about 63%, of the best possible choice of `k` groupings.

### Star-tree: layout

Apache Pinot's star-tree index materialises a cube selectively, inside each segment, as a tree over
pre-aggregated records ("documents"). It is configured with a dimension split order, the aggregates
to keep (such as `SUM__impressions`), and a leaf threshold `T` (`maxLeafRecords`, 10,000 by
default).

<figure class="diagram">
{{#include figures/data-cube-and-star-tree-index-star-tree.svg}}
<figcaption>Part of a star-tree with split order <code>country, browser, locale</code>; star nodes are dashed. Each node records the dimension it splits on, a range <code>[start, end)</code> of documents and one aggregated document for that range. The query below the tree follows the blue path: <code>country=US</code> for the predicate, the star node for <code>browser</code>, which it ignores, and every non-star <code>locale</code> child for the group-by.</figcaption>
</figure>

Documents are sorted by the split order, so each node's documents are a contiguous range. A star
child holds extra documents in which the node's split dimension is replaced by `*` and the metrics
are re-aggregated over it.

### Star-tree: build

1. Project the segment's rows onto the split-order dimensions and aggregate the metrics per
   distinct combination, a `GROUP BY` over all of them (see [partitioned hash
   table](../query-execution/partitioned-hash-table.md)).
2. Sort the resulting documents by the split order.
3. Start with a root node covering all documents, at the first dimension of the split order.
4. For a node covering more than `T` documents, at dimension `d`:
   1. Split its range into children, one per value of `d`. Because of the sort, each child is a
      contiguous run.
   2. Unless `d` has a single value in the range, or is listed in
      `skipStarNodeCreationForDimensions`, create a star child: copy the node's documents with `d`
      set to `*`, re-aggregate the copies that are now equal, sort them by the remaining
      dimensions, append them to the document array, and give the star child their range.
   3. Recurse into every child at the next dimension.
5. Compute each node's aggregated document over its range.

### Star-tree: query

For a query such as `SUM(impressions) WHERE country = 'US' GROUP BY locale`, walk the tree from the
root. At each node, look at its split dimension:

1. If the query has a predicate on it, follow the children whose value satisfies the predicate.
2. If the query groups by it (and has no predicate on it), follow every child except the star
   child.
3. If the query neither filters nor groups by it, follow the star child if there is one, otherwise
   every child.

At each node reached this way, if no dimension below it still has a predicate or a group-by, use
its aggregated document. Otherwise scan its documents, applying the remaining predicates and
grouping. Finally merge the partial aggregates with each aggregate's merge function.

Pinot's documentation lists equality, `IN`, `NOT IN`, ranges and `AND` as supported predicates,
`OR` only within one dimension, and no support for `IS NULL` or `REGEXP_LIKE`. A query outside
that set falls back to the raw rows.

### Variants

- **Ingestion-time rollup.** Apache Druid can aggregate rows with equal dimension values (at a
  chosen time granularity) as they are ingested. Only the rolled-up rows are kept, so queries on
  the dropped detail are impossible.
- **Aggregating storage.** ClickHouse's `AggregatingMergeTree` stores aggregate states and merges
  rows with equal keys during background merges; a projection can store an extra pre-aggregated
  copy of each part, which the planner uses when a query fits it.
- **MOLAP arrays.** Multidimensional OLAP servers store a cube as an array indexed by dimension
  codes. Gray et al. describe such an array with one extra index per dimension for `ALL`, and
  compute each coarser slab by aggregating one dimension of the finer one.

## Invariants

- Every node's aggregated document equals the aggregate of the raw rows matching the node's path,
  where `*` matches any value.
- A node's non-star children partition its documents by the value of its split dimension; a star
  child's aggregate equals its parent's aggregate.
- The aggregated document of the root equals the aggregate over the whole segment.
- Documents inside every node's range are sorted by the remaining split dimensions.
- For every supported query, the star-tree answer equals `GROUP BY` over the raw rows.

## Implementation pitfalls

- **Averaging averages.** The average of group averages is wrong whenever groups differ in size.
  Store `(sum, count)` and divide at the end.
- **Holistic aggregates.** An exact `COUNT(DISTINCT)` or median cannot be merged; Pinot's
  documentation lists exact distinct count and exact percentile as unsupported for this reason.
  Offer them only through sketches, and say the result is approximate.
- **Double counting.** A traversal must never take both the star child and a value child for the
  same dimension, and the star child's appended documents must never fall inside a non-star node's
  range. Either mistake counts rows twice.
- **NULL is not `*`.** A NULL dimension value is a real group; `*` means "all values". Represent
  `*` with its own enum variant (for example `Dim::Star` beside `Dim::Value(Option<V>)`), not with
  NULL or a magic string.
- **Overflow in sums.** Pre-aggregation adds many rows into one value, so overflow is more likely
  than in a single row. In this crate clippy denies unchecked arithmetic: use `checked_add` and
  report overflow, or aggregate into a wider type.
- **Floating-point sums.** Different aggregation orders give slightly different float sums. Compare
  against the reference with a tolerance, or test with integer metrics.
- **Storage blow-up.** The cube grows with `∏ (Cᵢ + 1)`. Put high-cardinality dimensions late in
  the split order or skip their star nodes, and tune `T`: a small `T` pre-aggregates more and stores
  more.
- **Deletes and `MIN`/`MAX`.** A sum can be decremented when a row disappears; a minimum cannot.
  Pre-aggregates over mutable data must be rebuilt, which is why they fit immutable segments.

## Testing

- Against `GROUP BY` on raw rows: for random data and random queries (subsets of dimensions as
  equality, `IN` and range predicates, and as group-by columns), compare the star-tree answer with a
  `HashMap`-based `GROUP BY` over the raw rows. This is the test that matters.
- Structural checks after every build: the invariants above, plus a `COUNT(*)` with no predicates,
  answered through the tree, equal to the segment's row count.
- Extremes of `T`: with `T = 1` every combination is pre-aggregated (Pinot's documentation uses this
  in its example); with `T` above the row count, the tree is a single leaf and every query scans.
- Edge cases: an empty segment, a single row, a dimension with one value (no star node), NULL
  dimension values, and metric values near the integer limits.

## Further reading

- [Data Cube: A Relational Aggregation Operator Generalizing Group-By, Cross-Tab, and
  Sub-Totals](https://arxiv.org/abs/cs/0701155), Gray et al., 1997.
- Harinarayan, Rajaraman and Ullman, *Implementing Data Cubes Efficiently*, SIGMOD 1996.
- [Star-tree index](https://docs.pinot.apache.org/basics/indexing/star-tree-index), Apache Pinot
  documentation.
