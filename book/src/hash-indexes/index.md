# Hash indexes

Indexes that answer point lookups with a hash of the key. They are fast and simple, but they lose
key order, so they cannot serve range scans or `ORDER BY`. The on-disk variants differ in how they
grow without rehashing the whole table.

- [Bitcask hash index](bitcask-hash-index.md)
- [Extendible hashing](extendible-hashing.md)
- [Linear hashing](linear-hashing.md)
