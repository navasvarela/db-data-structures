# Analytical query execution

The in-memory structures an analytical query engine runs on: batches of column values flowing
between operators, hash tables for joins and grouping, heaps for top-k, and trees for window
functions. They are designed around CPU caches and parallelism more than around disks.

- [Columnar batch](columnar-batch.md)
- [Partitioned hash table](partitioned-hash-table.md)
- [Bounded top-k heap](bounded-top-k-heap.md)
- [Segment tree](segment-tree.md)
