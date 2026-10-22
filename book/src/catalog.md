# Catalog and roadmap

Every structure in this book, with its status in the crate and the order in which
the structures build on each other.

- **Done**: implemented on `main`.
- **In progress**: started on `main` or on a branch.
- **Planned**: not started.

## Summary

| Group | Structure | Status |
| --- | --- | --- |
| [Storage primitives](storage-primitives/index.md) | [Append-only log](storage-primitives/append-only-log.md) | Done |
| | [Write-ahead log](storage-primitives/write-ahead-log.md) | Planned |
| | [Slotted page](storage-primitives/slotted-page.md) | Planned |
| | [Heap file and free space map](storage-primitives/heap-file-and-free-space-map.md) | Planned |
| | [Buffer pool](storage-primitives/buffer-pool.md) | Planned |
| | [Ring buffer](storage-primitives/ring-buffer.md) | Planned |
| [In-memory indexes](in-memory-indexes/index.md) | [Hash table](in-memory-indexes/hash-table.md) | Done |
| | [Skip list](in-memory-indexes/skip-list.md) | Done (dependency) |
| | [Red-black tree](in-memory-indexes/red-black-tree.md) | In progress |
| | [Adaptive radix tree](in-memory-indexes/adaptive-radix-tree.md) | Planned |
| | [Learned index](in-memory-indexes/learned-index.md) | Planned |
| [Hash indexes](hash-indexes/index.md) | [Bitcask hash index](hash-indexes/bitcask-hash-index.md) | Done |
| | [Extendible hashing](hash-indexes/extendible-hashing.md) | Planned |
| | [Linear hashing](hash-indexes/linear-hashing.md) | Planned |
| [Log-structured merge trees](lsm-trees/index.md) | [Memtable](lsm-trees/memtable.md) | In progress |
| | [SSTable](lsm-trees/sstable.md) | In progress |
| | [K-way merge heap](lsm-trees/k-way-merge-heap.md) | Planned |
| | [LSM tree](lsm-trees/lsm-tree.md) | Planned |
| [Page-based trees](page-based-trees/index.md) | [B+ tree](page-based-trees/b-plus-tree.md) | Planned |
| | [B-link tree](page-based-trees/b-link-tree.md) | Planned |
| | [Bε-tree](page-based-trees/b-epsilon-tree.md) | Planned |
| | [Bw-tree](page-based-trees/bw-tree.md) | Planned |
| | [Copy-on-write B+ tree](page-based-trees/copy-on-write-b-plus-tree.md) | Planned |
| [Filters](filters/index.md) | [Bloom filter](filters/bloom-filter.md) | Planned |
| | [Cuckoo filter](filters/cuckoo-filter.md) | Planned |
| | [Xor and Ribbon filters](filters/xor-and-ribbon-filters.md) | Planned |
| | [Range filter](filters/range-filter.md) | Planned |
| [Sketches and statistics](sketches/index.md) | [HyperLogLog](sketches/hyperloglog.md) | Planned |
| | [Theta sketch](sketches/theta-sketch.md) | Planned |
| | [Count-min sketch](sketches/count-min-sketch.md) | Planned |
| | [t-digest](sketches/t-digest.md) | Planned |
| | [KLL sketch](sketches/kll-sketch.md) | Planned |
| | [Reservoir sample](sketches/reservoir-sample.md) | Planned |
| | [Histogram](sketches/histogram.md) | Planned |
| [Analytical storage](analytical-storage/index.md) | [Columnar segments](analytical-storage/columnar-segments.md) | Planned |
| | [Column encodings](analytical-storage/column-encodings.md) | Planned |
| | [Floating-point and string compression](analytical-storage/floating-point-and-string-compression.md) | Planned |
| | [Zone maps](analytical-storage/zone-maps.md) | Planned |
| | [Sparse primary index](analytical-storage/sparse-primary-index.md) | Planned |
| | [Bitmap index](analytical-storage/bitmap-index.md) | Planned |
| | [Deletion vector](analytical-storage/deletion-vector.md) | Planned |
| | [Delta and main store](analytical-storage/delta-and-main-store.md) | Planned |
| | [Table snapshot manifest](analytical-storage/table-snapshot-manifest.md) | Planned |
| | [Data cube and star-tree index](analytical-storage/data-cube-and-star-tree-index.md) | Planned |
| [Analytical query execution](query-execution/index.md) | [Columnar batch](query-execution/columnar-batch.md) | Planned |
| | [Partitioned hash table](query-execution/partitioned-hash-table.md) | Planned |
| | [Bounded top-k heap](query-execution/bounded-top-k-heap.md) | Planned |
| | [Segment tree](query-execution/segment-tree.md) | Planned |
| [Specialised indexes](specialised-indexes/index.md) | [Inverted index](specialised-indexes/inverted-index.md) | Planned |
| | [Finite state transducer](specialised-indexes/finite-state-transducer.md) | Planned |
| | [R-tree](specialised-indexes/r-tree.md) | Planned |
| | [Space-filling curves](specialised-indexes/space-filling-curves.md) | Planned |
| | [HNSW graph](specialised-indexes/hnsw-graph.md) | Planned |
| [Transactions and recovery](transactions/index.md) | [MVCC version chain](transactions/mvcc-version-chain.md) | Planned |
| | [Lock table and wait-for graph](transactions/lock-table-and-wait-for-graph.md) | Planned |
| | [Undo log](transactions/undo-log.md) | Planned |
| [Distribution](distribution/index.md) | [Consistent hashing ring](distribution/consistent-hashing-ring.md) | Planned |
| | [Merkle tree](distribution/merkle-tree.md) | Planned |
| | [Replicated log](distribution/replicated-log.md) | Planned |
| | [Logical clocks](distribution/logical-clocks.md) | Planned |

## Suggested order

The structures below build on each other, and the crate already has the first rungs of two
ladders.

**Log-structured path.** This reuses the byte stores and the record format.

1. [Memtable](lsm-trees/memtable.md) on top of the [red-black tree](in-memory-indexes/red-black-tree.md) or the
   [skip list](in-memory-indexes/skip-list.md).
2. [SSTable](lsm-trees/sstable.md): flush a memtable into sorted blocks with a sparse index.
3. [K-way merge heap](lsm-trees/k-way-merge-heap.md): merge SSTables for reads and compaction.
4. [Bloom filter](filters/bloom-filter.md): skip SSTables that cannot hold a key.
5. [Write-ahead log](storage-primitives/write-ahead-log.md): make the memtable survive a crash.
6. [LSM tree](lsm-trees/lsm-tree.md): tie the pieces together with levels, compaction and a manifest.

**Page-based path.** This needs fixed-size pages instead of length-prefixed records.

1. [Slotted page](storage-primitives/slotted-page.md).
2. [Buffer pool](storage-primitives/buffer-pool.md).
3. [B+ tree](page-based-trees/b-plus-tree.md).
4. [Write-ahead log](storage-primitives/write-ahead-log.md) with page images, then [B-link tree](page-based-trees/b-link-tree.md) for
   concurrency.

**Analytical path.** This needs no pages and no WAL to start, since analytical files are written
once and then only read.

1. [Column encodings](analytical-storage/column-encodings.md): varints, bit-packing, dictionary, run-length and delta.
2. [Columnar segments](analytical-storage/columnar-segments.md) with [zone maps](analytical-storage/zone-maps.md) in the footer.
3. [Columnar batch](query-execution/columnar-batch.md): scan a segment into vectors and filter them with a
   selection vector.
4. [Partitioned hash table](query-execution/partitioned-hash-table.md) and [bounded top-k heap](query-execution/bounded-top-k-heap.md):
   `GROUP BY`, joins and `ORDER BY ... LIMIT` over batches.
5. [Sparse primary index](analytical-storage/sparse-primary-index.md) and [bitmap index](analytical-storage/bitmap-index.md): skip rows
   instead of scanning them.
6. [HyperLogLog](sketches/hyperloglog.md) and [t-digest](sketches/t-digest.md): approximate `COUNT(DISTINCT)` and
   percentiles that merge across segments.
7. [Deletion vector](analytical-storage/deletion-vector.md), then [delta and main store](analytical-storage/delta-and-main-store.md): accept
   updates without giving up the read-optimised layout.
8. [Data cube and star-tree index](analytical-storage/data-cube-and-star-tree-index.md): pre-aggregate the common
   queries.

**Standalone structures.** Filters, the remaining sketches, the specialised indexes and the
distribution structures depend on nothing above, so they can be picked up in any order.
