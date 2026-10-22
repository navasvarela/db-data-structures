# In-memory indexes

Ordered and unordered maps that live entirely in memory. They serve as the memtable of an LSM tree,
the key directory of a hash index, and the working state of query operators. They also show the
basic trade-off of every index: a hash gives the fastest point lookups, but only an ordered
structure can answer range queries.

- [Hash table](hash-table.md)
- [Skip list](skip-list.md)
- [Red-black tree](red-black-tree.md)
- [Adaptive radix tree](adaptive-radix-tree.md)
- [Learned index](learned-index.md)
