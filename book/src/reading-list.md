# Reading list

Sources that cover many structures at once. Each structure's page lists the papers specific to it.

## Books

- Kleppmann and Riccomini,
  [*Designing Data-Intensive Applications*](https://www.oreilly.com/library/view/designing-data-intensive-applications/9781098119058/),
  2nd ed., O'Reilly, 2026. The best overview of why storage engines, replication and stream
  processing look the way they do. Start here.
- Petrov, [*Database Internals*](https://www.databass.dev/), O'Reilly, 2019. The first half is
  storage engines (B-trees, slotted pages, buffer management, LSM trees); the second is
  distributed systems (failure detection, replication, consensus). The closest single book to
  the scope of this one.
- Hellerstein, Stonebraker and Hamilton,
  [*Architecture of a Database System*](https://courses.cs.washington.edu/courses/cse544/20wi/papers/DBArchitecture.pdf),
  2007. How the parts of a relational database fit together: process model, storage, buffer
  pool, lock manager, log manager.
- Bailis, Hellerstein and Stonebraker (eds.), [*Readings in Database Systems*](http://www.redbook.io/),
  5th ed., 2015. The "Red Book": classic papers chosen and introduced by the editors. Free online.
- Abadi, Boncz, Harizopoulos, Idreos and Madden,
  [*The Design and Implementation of Modern Column-Oriented Database Systems*](https://ir.cwi.nl/pub/21772/1900000024-Abadi-Vol5-DBS-024.pdf),
  2013. The standard introduction to column stores: layouts, compression, late materialisation,
  vectorised execution.
- Graefe, [*Modern B-Tree Techniques*](https://nowpublishers.com/article/Details/DBS-028), 2011,
  and its 2024 sequel *More Modern B-Tree Techniques*. Everything about B-trees, from page layout
  to locking and recovery.
- Gray and Reuter, *Transaction Processing: Concepts and Techniques*, Morgan Kaufmann, 1992. The
  classic on logging, locking and recovery, with implementation-level detail.
- Sedgewick and Wayne, *Algorithms*, 4th ed., Addison-Wesley, 2011. Balanced trees, hash tables,
  heaps and tries, with clear implementations.
- Slotin, [*Algorithms for Modern Hardware*](https://en.algorithmica.org/hpc/). How caches, SIMD
  and branch prediction change the design of searches, B-trees and hash tables. Free online.
- [*Learn Rust With Entirely Too Many Linked Lists*](https://rust-unofficial.github.io/too-many-lists/).
  How ownership shapes pointer-based structures in Rust. Worth reading before writing trees in
  safe code.

## Courses

- [CMU 15-445/645: Database Systems](https://15445.courses.cs.cmu.edu/fall2025/), Pavlo et al. The
  introductory course: storage, buffer pools, hash tables, B+ trees, query execution,
  concurrency control and recovery. Lectures are free on the
  [CMU Database Group YouTube channel](https://www.youtube.com/@CMUDatabaseGroup), and its
  projects are built on the [BusTub](https://github.com/cmu-db/bustub) teaching database.
- [CMU 15-721: Advanced Database Systems](https://15721.courses.cs.cmu.edu/), Pavlo. The
  analytical follow-up: storage formats, compression, vectorised execution, parallel joins and
  case studies of modern OLAP systems, each with a reading list of papers.

## Build-along projects

- [Mini-LSM](https://skyzh.github.io/mini-lsm/), Chi. Build an LSM storage engine in Rust over
  three weeks of exercises: memtable, SSTables, merge iterators, compaction, WAL, MVCC.
- [Build Your Own Database From Scratch](https://build-your-own.org/database/), Ye. A copy-on-write
  B+ tree and a small relational database on top, in Go. The first part is free online.
- [toyDB](https://github.com/erikgrinaker/toydb), Grinaker. A distributed SQL database in Rust,
  with Raft, MVCC and a pluggable storage engine. Its
  [architecture guide](https://github.com/erikgrinaker/toydb/blob/main/docs/architecture/index.md)
  walks through the code.
- [Let's Build a Simple Database](https://cstack.github.io/db_tutorial/), Stack. A SQLite clone in
  C, as far as a B-tree with pages on disk.

## Real systems, documented

- [The Internals of PostgreSQL](https://www.interdb.jp/pg/), Suzuki. Heap pages, the buffer
  manager, MVCC, vacuum and the WAL, as PostgreSQL implements them.
- [SQLite database file format](https://sqlite.org/fileformat2.html). A complete, byte-level
  specification of a B-tree database file.
- [LevelDB documentation](https://github.com/google/leveldb/blob/main/doc/impl.md) and the
  [RocksDB wiki](https://github.com/facebook/rocksdb/wiki). The reference LSM engine and its
  production descendant.
- [ClickHouse primary indexes](https://clickhouse.com/docs/guides/best-practices/sparse-primary-indexes),
  [Apache Arrow columnar format](https://arrow.apache.org/docs/format/Columnar.html) and the
  [Apache Iceberg specification](https://iceberg.apache.org/spec/). Analytical storage specified
  in detail.
- [DuckDB research library](https://duckdb.org/library/). The papers behind an embedded analytical
  database, many of them about structures in this book.

## Surveys and paper digests

- Luo and Carey, [*LSM-based Storage Techniques: A Survey*](https://arxiv.org/abs/1812.07527),
  2019.
- [The Morning Paper](https://blog.acolyer.org/), Colyer. An archive of short summaries of
  systems and database papers, many of them cited in this book.
