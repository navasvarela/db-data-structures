# Log-structured merge trees

An LSM tree buffers writes in memory, flushes them as immutable sorted files, and merges those files
in the background. It turns random writes into sequential ones, at the cost of reads that may have
to look in several places. Most modern key-value stores are built this way.

- [Memtable](memtable.md)
- [SSTable](sstable.md)
- [K-way merge heap](k-way-merge-heap.md)
- [LSM tree](lsm-tree.md)
