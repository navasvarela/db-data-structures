# Partitioned hash table

> Builds on [hash table](../in-memory-indexes/hash-table.md) and [columnar
> batch](../query-execution/columnar-batch.md).

A hash table built for query execution: the build side of a hash join, or the running state of a
`GROUP BY`. Radix partitioning first splits the input by a few hash bits so each partition's table
fits in cache. Parallel aggregation pre-aggregates into thread-local tables, then merges them one
partition at a time.

- **Used by:** DuckDB, HyPer and Umbra, ClickHouse aggregation, Velox.
- **Cost:** expected `O(1)` per row. Open addressing with linear probing, plus a few stored hash
  bits per slot to reject mismatches without comparing keys, is the common layout.
- **Notes:** Balkesen et al., 2013 (radix hash joins); Leis et al., 2014 (morsel-driven
  parallelism).

## How it works

### Layout

Every row's key is hashed once, to a 64-bit value. Different bits of that hash do different jobs:
the top `p` bits choose one of `2^p` partitions, some of the remaining bits choose a slot inside
that partition's table, and a few more are kept in the slot as a *tag* (also called a *salt*) to
reject most non-matching keys without comparing them.

<figure class="diagram">
{{#include figures/partitioned-hash-table-layout.svg}}
<figcaption>One hash, three jobs. This key belongs to partition 2, its home slot there is 1, and slot 1 stores its tag <code>0x3a</code> with a reference to row 17. Slots are small; the keys and payloads stay in separate row storage.</figcaption>
</figure>

Each partition has its own table: an array of slots holding a tag and a reference to a row, plus
separate row storage holding the key columns and either the payload columns (for a join) or the
running aggregate states (for a `GROUP BY`). Keeping slots small and rows elsewhere means probing
reads a compact array, and resizing moves only slots, not rows.

The partition count is picked so that one partition's table fits in a CPU cache. Probing a table
that fits in cache costs a cache hit per lookup; probing a table much larger than the cache costs a
miss on almost every lookup.

### Building the join table

The *build side* is the input the table is made from, usually the smaller one; the *probe side* is
streamed past it.

1. Read the build side batch by batch. For each row, compute the key hash and copy the row, with its
   hash, into a buffer for its partition. With several threads, each thread keeps its own set of
   partition buffers, so this step needs no synchronisation.
2. When the build side is exhausted, the exact row count of every partition is known. Allocate
   each partition's slot array at a power of two that keeps the load factor below the chosen
   maximum.
3. Insert every buffered row: compute its slot from the hash, and probe forward from there
   (linear probing) until an empty slot is found. Write the tag and the row reference.

Leis et al. (2014) build HyPer's join table in the same two phases, and note that knowing the
exact size before creating the table avoids growing it while many threads insert.

### Probing the join table

1. For each probe row, hash the key with the same function, and pick the partition and slot.
2. Walk slots from there. Skip a slot whose tag differs. For a slot whose tag matches, compare the
   full key, and emit a match on equality.
3. Keep walking until an empty slot: with duplicate build keys, one probe row can match many build
   rows.
4. Collect matches as two index lists (probe row, build row) and gather the output batch from them
   once per batch, rather than copying row by row.

<figure class="diagram">
{{#include figures/partitioned-hash-table-probe.svg}}
<figcaption>Probing partition 2 of the table above with a key whose tag is <code>0x91</code>. Slot 1 is skipped on its tag alone. Slots 2 and 3 both carry the tag, so the full keys are compared: row 4 matches and row 9 does not. The empty slot 4 ends the probe.</figcaption>
</figure>

Semi, anti and outer joins add a "matched" flag per build row, set during the probe; a final pass
over the build side emits the rows whose flag is still clear (for a right outer or anti join).

### Aggregating with `GROUP BY`

Aggregation cannot stream its output: no group is final until all input is read. The parallel
version has two phases.

1. **Pre-aggregate.** Each thread aggregates its input into a small, fixed-size, thread-local
   table. Frequent keys collapse into one entry there. When the table fills, its entries are
   flushed into the thread's partition buffers, chosen by hash bits, and the table is cleared.
2. **Merge per partition.** After all input is consumed, each partition is handed to one thread,
   which combines the partial states from every thread's buffer for that partition into one table.
   Since a key always hashes to the same partition, no two threads ever merge the same group.
3. **Finalise.** For each group, turn the state into the result: `avg` divides its sum by its count,
   for example. Emit the partition's groups as batches before moving to the next partition.

<figure class="diagram">
{{#include figures/partitioned-hash-table-aggregate.svg}}
<figcaption>Two-phase parallel aggregation with four partitions. Each thread flushes its local table into its own partition buffers; afterwards one thread merges partition 2's buffers from every thread, so a group is never merged by two threads.</figcaption>
</figure>

This is the scheme of the morsel-driven paper (Leis et al., 2014). DuckDB's parallel aggregation
is similar: a thread starts with one local table and switches to partitioned tables only once its
table grows past a fixed number of entries, so queries with few groups never pay for
partitioning.

### Radix partitioning

Writing rows into `2^p` partition buffers is itself a scatter. Partitions sit on different memory
pages, and once the fan-out exceeds the number of entries in the CPU's translation lookaside buffer
(TLB, the cache of virtual-to-physical page mappings), almost every write costs a TLB miss. Radix
partitioning, as described by Manegold et al. and evaluated by Balkesen et al. (2013), handles a
large fan-out in more than one pass: a first pass splits on some hash bits, and a second pass
splits each piece on the next bits. Balkesen et al. also write through small per-partition buffers
that fit in the L1 cache, and copy a buffer to its partition only when it is full, which raises the
fan-out one pass can afford.

### Variants

- **Chaining with tagged pointers:** HyPer's join table is an array of pointers to chains of rows,
  with a 16-bit tag stored in unused pointer bits, so an empty chain or a non-matching key is
  usually rejected without following the pointer.
- **Single shared table versus radix-partitioned join:** the morsel paper uses one table for the
  join and partitions only for aggregation, observing that in benchmarks such as TPC-H almost all
  joined tuples come from the probe side, so the build table is often small enough to stay in cache.
  Radix joins pay off when both sides are large.
- **Spilling (grace hash join):** when the partitions do not fit in memory, write them to disk and
  process one partition pair at a time. Partitioning both sides with the same hash bits is what
  makes the per-partition join correct.

## Invariants

- A row's partition and slot depend only on its key's hash, and the hash depends only on the key's
  value.
- Every build row is in exactly one partition and reachable from exactly one slot.
- No partition's load factor exceeds the configured maximum.
- For linear probing: between a key's home slot and its actual slot there is no empty slot.
- After aggregation, every group key appears in exactly one entry of exactly one partition, and the
  sum of all `COUNT(*)` states equals the number of input rows.

## Implementation pitfalls

- **`NULL` keys in joins.** In SQL, `NULL = NULL` is not true, so a `NULL` join key matches nothing.
  Skip such rows at build time for an inner join, but keep them for outer and anti joins, which must
  still output them. `NOT IN` is stricter: if the subquery returns any `NULL`, `x NOT IN (...)` is
  never true. `GROUP BY` is different again: all `NULL` keys form one group.
- **Reusing partition bits as slot bits.** After partitioning on the top bits of the hash, every key
  in a partition shares those bits. If the slot index is also taken from the top bits, all keys
  land in a few slots and probing becomes linear. Take slot and tag bits from parts of the hash that
  partitioning did not use.
- **Resizing during the build.** Growing a table while rows are being inserted moves every slot, and
  slot positions held elsewhere become stale. Materialising first and sizing once avoids it for
  joins. An aggregation table cannot know its group count in advance, so it must grow: do it between
  batches, never while a batch's insert positions are still in use, and keep the full hash in the
  row storage so growing does not re-hash keys. DuckDB's aggregate table does this, and its rows
  never move when the slot array is replaced.
- **Different hashers on different threads.** Each `RandomState::new()` in Rust gets its own keys,
  so two threads with their own `RandomState` hash the same key to different partitions, and
  aggregation produces duplicate groups. Share one hasher configuration, or use a fixed-seed hash.
  Anything spilled to disk and read back by another process needs a hash with a fixed
  specification, not `DefaultHasher` or `RandomState`.
- **Type mismatches between sides.** Joining an `i32` column with an `i64` column must hash both as
  the same type, or equal values hash differently and never meet. Floats need care too: `-0.0` and
  `0.0` compare equal but have different bits, and the engine must decide how `NaN` groups.
- **Tag matches are not key matches.** A tag of 8 or 16 bits collides often enough that skipping the
  full key comparison produces wrong joins in any large test.
- **Skewed keys.** With linear probing, one build key with millions of duplicates fills millions of
  consecutive slots, and every probe that lands in that run walks it, even for other keys. Chaining
  duplicates behind one slot keeps probes short.
- **Merging aggregate states.** States from different threads must be combined, not finalised and
  then combined: the average of two averages is wrong unless the counts are equal. Store `avg` as
  a sum and a count.
- **Overflow in states.** A `SUM` over `i64` can overflow. With clippy denying overflowing
  arithmetic, use `checked_add` and report the error, or accumulate in a wider type.
- **Bit extraction.** Shifting a `u64` by 64 is an overflow, which happens when `p = 0`. Use
  `checked_shr` or handle zero partition bits separately, and compute slots with a mask
  (`hash & (capacity - 1)`) rather than `%`.

## Testing

- Reference models: a nested-loop join over two `Vec`s, and a `BTreeMap` from key to aggregate for
  `GROUP BY`. Compare results as multisets, since the hash table's output order is arbitrary.
- Property tests: random inputs with many duplicate keys, many `NULL` keys and empty inputs on
  either side, run through every join type (inner, left, right, semi, anti).
- Split invariance: aggregate the same input in one thread, and again split at random points across
  several simulated threads with tiny pre-aggregation tables that flush constantly; results must be
  identical.
- Adversarial hashes: plug in a hash function that returns the same value, or values that differ
  only in the partition bits, and check that results are still correct (only slower).
- Edge cases: `p = 0` (one partition), a partition that receives no rows, a table filled exactly to
  the load-factor limit, and keys that differ only beyond the tag bits.

## Further reading

- [Morsel-Driven Parallelism: A NUMA-Aware Query Evaluation Framework for the Many-Core
  Age](https://db.in.tum.de/~leis/papers/morsels.pdf), Leis, Boncz, Kemper and Neumann, 2014.
- [Multi-Core, Main-Memory Joins: Sort vs. Hash
  Revisited](https://www.vldb.org/pvldb/vol7/p85-balkesen.pdf), Balkesen et al., 2013.
- [Parallel Grouped Aggregation in DuckDB](https://duckdb.org/2022/03/07/aggregate-hashtable.html),
  DuckDB blog, 2022.
- [A Seven-Dimensional Analysis of Hashing
  Methods](https://www.vldb.org/pvldb/vol9/p96-richter.pdf), Richter, Alvarez and Dittrich, 2015.
