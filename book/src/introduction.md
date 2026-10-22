# Introduction

This book explains the data structures that databases are built from: how a key-value store lays
out its files, how an index finds a row, how an analytical engine scans a billion values, and how
replicas agree on what happened. It is the companion guide to the
[`db-data-structures`](https://github.com/navasvarela/db-data-structures) crate, which implements
these structures in Rust, one at a time, to be read and learned from as much as used.

## Two kinds of workload

Most of the structures fall on one side of a split that shapes every database.

- **Transactional (OLTP)** systems such as PostgreSQL, MySQL or RocksDB serve many small reads
  and writes, each touching a handful of rows by key. They favour structures with fast point
  lookups and cheap, durable updates: logs, B-trees, LSM trees, hash indexes, MVCC.
- **Analytical (OLAP)** systems such as ClickHouse, DuckDB, Snowflake or Apache Druid serve fewer,
  much larger queries that read a few columns of many rows and aggregate them. They favour
  structures that avoid reading data at all, and that make what must be read small and fast to
  process: columnar layouts, compression, zone maps, bitmaps, vectorised batches, sketches.

Some structures serve both. A Bloom filter skips SSTables in a key-value store and skips
partitions in a data warehouse, and a hash table is both a key directory and the core of a hash
join.

## How the book is organised

Each chapter groups structures by the job they do, starting from bytes on disk and moving up to
query execution and distribution. Every page follows the same outline:

- what the structure is and the problem it solves;
- which real systems use it;
- what it costs in time, space and I/O;
- notes on the design decisions an implementation has to make;
- further reading, usually the original paper plus a more practical source.

The [catalog and roadmap](catalog.md) lists every structure with its status in the crate, and
suggests an order to build them in, since many depend on others. The
[reading list](reading-list.md) collects the books, courses and build-along projects that cover
many structures at once.

## The code

The crate's API documentation is published alongside this book, under
[`api/`](https://navasvarela.github.io/db-data-structures/api/db_data_structures/), and each
version released to crates.io is documented on [docs.rs](https://docs.rs/db-data-structures).
Pages for structures that are already implemented link to their source.

To build this book locally, install [mdBook](https://rust-lang.github.io/mdBook/) and run
`just book-serve` from the repository root.
