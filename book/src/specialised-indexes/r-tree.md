# R-tree

> Builds on [B+ tree](../page-based-trees/b-plus-tree.md)-style pages.

A balanced tree in which each node holds the bounding rectangles of its children. It answers "what
overlaps this region" and nearest-neighbour queries for spatial data.

- **Used by:** PostGIS (through GiST), SQLite's R*Tree module, MySQL spatial indexes.
- **Notes:** Guttman, 1984; the R*-tree (Beckmann et al., 1990) improves its split heuristics.

## How it works

### Layout

Every node is a page with between `m` and `M` entries, where `M` is what fits in a page and
`m ≤ M / 2` is the minimum fill. An entry is a pair:

- in a **leaf**: the *minimum bounding rectangle* (MBR) of one object, the smallest axis-aligned
  box that contains it, and the object's ID;
- in an **inner node**: the MBR of a child, which is the smallest box containing every entry in the
  child, and the child's page ID.

All leaves are at the same depth, as in a [B+ tree](../page-based-trees/b-plus-tree.md). Unlike a
B+ tree, sibling boxes may overlap, and there is no total order between entries, so a search may
have to follow several children.

<figure class="diagram">
{{#include figures/r-tree-layout.svg}}
<figcaption>Two leaves under one root. <code>R1</code> is the MBR of <code>a</code> and <code>b</code>, and <code>R2</code> of <code>c</code>, <code>d</code> and <code>e</code> (drawn with a small margin so the edges stay visible). The two boxes overlap, even though no object is in both.</figcaption>
</figure>

A rectangle in `d` dimensions is `d` pairs `(min, max)`. Most implementations use two or three
dimensions.

### Search (window query)

1. Start at the root with the query rectangle `Q`.
2. In an inner node, descend into every child whose MBR intersects `Q`.
3. In a leaf, report every entry whose MBR intersects `Q`. The MBR is only an approximation, so the
   caller then tests the exact geometry; this is the *refinement* step that PostGIS performs after
   its index scan, for example.

Containment queries ("objects inside `Q`") use the same descent, with the containment test at the
leaves.

<figure class="diagram">
{{#include figures/r-tree-window-query.svg}}
<figcaption>A window query <code>Q</code> in the overlap. Both root entries intersect it, so both leaves are read, and each contributes one result.</figcaption>
</figure>

### Insert

1. **Choose a leaf.** From the root, pick the child whose MBR would grow the least in area to
   include the new rectangle; break ties by the smallest area. Repeat down to a leaf.
2. Add the entry. If the leaf now has more than `M` entries, split it.
3. **Adjust the tree.** Walk back up the path: enlarge each parent entry's MBR to cover the change,
   and if a split happened, add an entry for the new node to the parent, which may split in turn.
4. If the root splits, create a new root with the two halves as children.

### Split

Splitting `M + 1` entries into two groups of at least `m` each, so that the two groups' MBRs are
small, is the heuristic part. Guttman's **quadratic split**:

1. **Pick seeds:** for every pair of entries, compute the area of the box containing both minus the
   two entries' own areas. Start the two groups with the pair that wastes the most.
2. **Pick next:** among the remaining entries, choose the one with the largest difference between
   the area increase it would cause in one group and in the other, and add it to the group that
   grows less (ties: smaller area, then fewer entries).
3. If one group needs all the remaining entries to reach `m`, give them to it and stop.

Guttman also gives a cheaper linear split, which picks seeds by their separation along each axis
and assigns the rest in any order.

### Delete

1. **Find the leaf.** Search for the entry by its MBR and object ID. Because boxes overlap, this may
   have to try several subtrees.
2. Remove the entry.
3. **Condense the tree.** Walk back up the path. If a node now has fewer than `m` entries, remove it
   from its parent and set its remaining entries aside; otherwise shrink its MBR in the parent to
   fit its remaining entries.
4. Reinsert the set-aside entries, each at the level it came from, so subtrees from inner nodes go
   back in as subtrees.
5. If the root is an inner node with one child, make the child the root.

Reinserting instead of merging with a sibling is deliberate: it lets entries find a better place in
the tree, and the code is shared with insert.

### Nearest-neighbour query

Best-first search (Hjaltason and Samet) finds the `k` nearest objects to a point `p`:

1. Push the root onto a min-priority queue keyed by `MINDIST(p, MBR)`, the distance from `p` to the
   nearest point of the box (zero if `p` is inside).
2. Pop the closest item. If it is a node, push all its entries with their `MINDIST`. If it is an
   object, it is the next nearest neighbour (after refinement with its exact distance, if the MBR
   is not the object itself).
3. Stop after `k` objects.

No box can contain anything closer to `p` than its `MINDIST`, so objects come out in distance order.

### Bulk load

Inserting one object at a time gives overlapping, poorly packed nodes. For a static data set,
Sort-Tile-Recursive (STR) packs the tree bottom-up: sort the rectangles by the x of their centres,
cut them into vertical slices, sort each slice by y, fill leaves of `M` entries from each slice, and
repeat the process on the leaves' MBRs to build each level above. Sorting by a
[Hilbert curve](space-filling-curves.md) value instead gives the Hilbert-packed R-tree.

### Variants

- **R\*-tree:** chooses the subtree at the level above the leaves by the least *overlap* increase
  rather than area; chooses the split axis by the smallest total perimeter (margin) of candidate
  splits, then the split position by the least overlap; and on the first overflow at each level of
  an insert, removes about 30% of the node's entries, those farthest from its centre, and
  reinserts them (*forced reinsert*). The paper found a minimum fill of 40% worked best. SQLite's
  R\*Tree module implements this variant.
- **GiST:** PostgreSQL's generalised search tree turns the R-tree's operations (consistent, union,
  penalty, pick split) into an interface, and PostGIS implements an R-tree on top of it.

## Invariants

- Every non-root node has between `m` and `M` entries; an inner root has at least two children.
- All leaves are at the same depth.
- Every inner entry's MBR contains every MBR in its child. After deletes it should also be tight
  (equal to their union); a box that is too large is still correct but slower, while one that is
  too small loses results.
- Every object appears in exactly one leaf entry.
- A window query returns exactly the objects whose MBRs intersect the window, whatever the shape of
  the tree.

## Implementation pitfalls

- **Floating-point comparisons.** `f64` is not `Ord`, and `NaN` breaks every comparison-based
  choice. Reject `NaN` coordinates at insert, and use `f64::total_cmp` for sorting and in the
  priority queue of nearest-neighbour search.
- **Rounding boxes.** If coordinates are stored at lower precision than they are computed, a box
  can end up smaller than the object. SQLite stores coordinates as 32-bit floats and rounds lower
  bounds down and upper bounds up, so boxes can only grow.
- **Degenerate rectangles.** Points and lines have zero area, so area-based choices tie everywhere.
  Fall back to the perimeter or the distance between centres to break ties, and test data sets of
  identical points.
- **Integer coordinates.** With integer coordinates, the product in an area calculation overflows
  easily. Compute areas in a wider type, or with `checked_mul`, which this crate's lints require
  anyway.
- **Delete that searches only one path.** The entry may be under any child whose box contains its
  MBR. Searching only the first such child fails to find it.
- **Reinsertion at the wrong level.** Entries set aside from an inner node are subtrees, and must be
  inserted at the same height, or leaves end up at different depths.
- **Forced reinsert loops.** The R\*-tree reinserts at most once per level per insertion; without
  that flag, reinsertion can overflow the same node again indefinitely.
- **Splits that violate the minimum.** Each split group must get at least `m` entries; the quadratic
  split's step 3 exists for this reason.

## Testing

- **Brute-force model.** Keep all rectangles in a `Vec`. For random query windows and points, check
  window queries (as sets) and `k`-nearest-neighbour queries (as sorted distance lists, since ties
  can come back in any order) against a linear scan.
- **Random workloads with a validator.** Interleave inserts and deletes with a small `M` (4 or 5),
  and after each operation walk the tree and check fill bounds, equal leaf depth, and MBR
  containment and tightness.
- **Skewed and degenerate data.** Clustered data, all points on a line, many identical points, very
  large boxes covering everything, and tiny boxes.
- **Bulk load.** Check that a bulk-loaded tree answers queries the same as one built by inserts,
  and passes the validator.
- **Quality metrics.** Count nodes visited per query and total overlap between sibling boxes, to
  compare split heuristics. These are not correctness checks, but they show whether a change made
  the tree better or worse.

## Further reading

- [R-Trees: A Dynamic Index Structure for Spatial Searching](https://doi.org/10.1145/602259.602266),
  Guttman, 1984.
- [The R*-tree: An Efficient and Robust Access Method for Points and
  Rectangles](https://doi.org/10.1145/93597.98741), Beckmann, Kriegel, Schneider and Seeger, 1990.
