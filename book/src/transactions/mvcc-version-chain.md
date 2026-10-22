# MVCC version chain

Each row keeps several versions tagged with the transactions that created and deleted them. A reader
sees the newest version visible to its snapshot, so readers never block writers.

- **Used by:** PostgreSQL (versions in the heap, cleaned by vacuum), InnoDB (old versions rebuilt
  from the undo log), CockroachDB and RocksDB-based engines (versions encoded in keys).
- **Notes:** garbage collection of old versions is the hard part.

## How it works

This page describes snapshot isolation built on timestamps, which is the easiest form to implement
and test, and then shows how PostgreSQL's transaction-id snapshots map onto it.

### Layout

Every transaction gets a *start timestamp* when it begins, and a *commit timestamp* when it
commits. Both come from one counter that only goes up. Each version of a row records the interval
of commit timestamps during which it was the current version:

- `begin`: the commit timestamp of the transaction that created it, or, while that transaction is
  still running, a marker naming the transaction;
- `end`: the commit timestamp of the transaction that replaced or deleted it, `∞` if it is still
  current, or a marker naming the transaction that is replacing it;
- the row's values (or, for a delete, a *tombstone* saying the row no longer exists);
- a pointer to the next version in the chain.

<figure class="diagram">
{{#include figures/mvcc-version-chain-layout.svg}}
<figcaption>The chain of key 42, newest first. Transaction 17 is replacing the balance of 100 with 70 and has not committed, so its marker stands in the <code>begin</code> of the new version and the <code>end</code> of the old one.</figcaption>
</figure>

Wu et al. (2017) classify the real designs along three axes, all of which change where the chain
lives but not the rules below:

- **Version storage.** *Append-only*: every version is a full row in the table (PostgreSQL).
  *Time-travel*: the table holds one version and older ones go to a separate area. *Delta*: the
  table holds the newest version and older ones are rebuilt from deltas in a rollback area (InnoDB
  and Oracle, through the [undo log](undo-log.md)).
- **Chain order.** *Oldest-to-newest* (PostgreSQL, Hekaton): the head is the oldest version, so a
  reader of current data walks the whole chain. *Newest-to-oldest*: the head is the newest version,
  so current reads stop at once, but every update has to repoint the index at the new head.
- **Garbage collection**, described below.

### Read with a snapshot

A transaction's *snapshot* is its start timestamp `ts`. It sees exactly the transactions that
committed with a commit timestamp at or below `ts`, plus its own writes.

1. Find the chain for the key through the index.
2. Walk the chain. For each version:
   - if `begin` names this transaction itself, the version is its own write: visible, unless `end`
     also names this transaction (it deleted or replaced its own write);
   - if `begin` names another transaction that has not committed, skip it;
   - if `begin <= ts`, the version is visible when its `end` is later than `ts`, or names another
     transaction that has not committed, or names one that committed after `ts`. If `end` names
     this transaction, it has replaced the version, so the version is not visible to it.
3. Return the first visible version. If it is a tombstone, or no version is visible, the row does
   not exist for this snapshot.

At most one version per key can satisfy the rule, because the committed intervals do not overlap.

<figure class="diagram">
{{#include figures/mvcc-version-chain-snapshots.svg}}
<figcaption>The same chain as intervals of commit timestamps, assuming transaction 17 started at 31. A snapshot at 20 sees 80. A snapshot at 38 sees 100, because transaction 17 has not committed. Transaction 17 itself sees its own write, 70.</figcaption>
</figure>

### Write

To update or delete key `k` in transaction `T` with start timestamp `ts`:

1. Find the newest version of `k`.
2. **Write-write conflict check.** If the newest version was written by another transaction that
   has not finished, someone else is changing the row: wait for that transaction, or abort `T` at
   once. If the other transaction aborts, retry from step 1; if it commits, abort `T`. If the
   newest version is committed with `begin > ts`, a concurrent transaction changed the row after
   `T`'s snapshot was taken: abort `T`. This is the rule that prevents lost updates.
3. Set the newest version's `end` to `T`'s marker. This must be an atomic compare-and-set from `∞`,
   so two writers that pass step 2 at the same moment cannot both claim the same version.
4. Create the new version with `begin = T`'s marker and `end = ∞`, and link it into the chain.
5. Record the change in `T`'s write set, so commit and abort can find it.

An insert is a write whose "newest version" is a tombstone or nothing at all; it still needs the
conflict check, or two transactions can insert the same key.

### Commit and abort

1. **Commit.** Take a commit timestamp `c` from the counter. For every version in the write set,
   replace `T`'s marker with `c`, in `begin` of new versions and `end` of old ones. Then mark `T`
   committed.
2. **Abort.** For every version in the write set, unlink the new version and reset the old
   version's `end` from `T`'s marker back to `∞`.

<figure class="diagram">
{{#include figures/mvcc-version-chain-commit.svg}}
<figcaption>Transaction 17 commits and takes commit timestamp 41. Both places that held its marker now hold 41: the balance of 70 is current from 41 on, and 100 was current from 30 to 41.</figcaption>
</figure>

The commit has to look atomic to readers. A snapshot taken between "got timestamp `c`" and "finished
stamping" could otherwise see some of `T`'s writes and not others. Two common fixes: a reader that
meets a marker looks up that transaction's state in a shared table and waits while it is
committing; or start timestamps are handed out only up to the last commit whose stamping has
finished.

### Garbage collection

A version is garbage once no current or future snapshot can see it.

1. Compute the *low watermark*: the smallest start timestamp among running transactions (or the
   current timestamp if none run).
2. A version whose `end` is a commit timestamp at or below the watermark is invisible to every
   running transaction and to every future one, since they start later. Unlink it and free it.
3. Keep the newest version visible at the watermark, even if a newer one exists, because some
   snapshot may still read it.
4. Remove index entries that point only at removed versions; a key whose only remaining version is
   an old tombstone can leave the index entirely.

<figure class="diagram">
{{#include figures/mvcc-version-chain-gc.svg}}
<figcaption>Collection after the commit, with the snapshot at 20 finished and the one at 38 still running, so the low watermark is 38. The version of 80 ended at 30 and no snapshot can see it. The version of 100 ended at 41, after the watermark, and the snapshot at 38 still reads it.</figcaption>
</figure>

Wu et al. distinguish *tuple-level* collection, either a background *vacuum* thread scanning for
dead versions (PostgreSQL, InnoDB) or *cooperative cleaning* by readers that pass dead versions
(Hekaton), from *transaction-level* collection, which frees the versions a finished transaction
replaced once every snapshot that could see them has ended.

### PostgreSQL's snapshots

PostgreSQL does not use commit timestamps. Each row version stores `xmin`, the id of the
transaction that created it, and `xmax`, the id of the transaction that deleted or replaced it.
A snapshot records three things: the oldest transaction id still running, the next id not yet
assigned, and the list of ids in between that were running when the snapshot was taken. A creating
transaction counts as committed for the snapshot if it committed and is neither in that list nor
at or after the "next id". The visibility rule then follows the same shape as above, with "committed
before my snapshot" decided by the list instead of by comparing timestamps. Under `REPEATABLE READ`
the snapshot is taken once per transaction; under `READ COMMITTED`, once per statement.

### Variants

- **Snapshot isolation versus serialisable.** The rules above give snapshot isolation, which still
  allows *write skew*: two transactions read overlapping data, write different rows, and together
  break a constraint neither broke alone. PostgreSQL documents this as a serialisation anomaly
  possible at `REPEATABLE READ`. Its `SERIALIZABLE` level adds Serializable Snapshot Isolation,
  which tracks read-write dependencies and aborts transactions that could form a cycle.
- **Versions in keys.** CockroachDB and engines on RocksDB append the commit timestamp to the key,
  so the chain is a run of adjacent keys in sorted order and the [LSM
  tree](../lsm-trees/lsm-tree.md) is the version store.

## Invariants

- For each key, the committed versions' intervals `[begin, end)` do not overlap and leave no gaps
  between the oldest kept version and `∞`.
- Along a newest-to-oldest chain, `begin` strictly decreases.
- Each key has at most one uncommitted version, and it is the newest.
- A snapshot that reads the same key twice gets the same version, unless it wrote the key in
  between.
- A committed version's values and `begin` never change; only `end` is set, once.
- Garbage collection never removes a version visible to any running transaction.

## Implementation pitfalls

- **Visibility of a transaction's own writes.** A transaction must see its own uncommitted writes,
  which the timestamp rule alone does not give. Within one statement it may need to *not* see them:
  an `UPDATE ... SET x = x + 1` that scans the rows it is updating would see its new versions and
  update them again. PostgreSQL stores a command id in each version for this.
- **First-updater versus first-committer wins.** Checking for conflicts at write time, as above,
  makes the second writer wait for the first and then abort if the first commits. That is
  PostgreSQL's behaviour at `REPEATABLE READ`, where the error is "could not serialize access due to
  concurrent update". Checking only at commit time also prevents lost updates, but the loser does
  all its work before finding out. Either way, a check that ignores uncommitted versions, combined
  with a plain store instead of a compare-and-set in step 3, lets two writers both pass, and one
  update is lost.
- **Inserts conflict too.** Two transactions inserting the same primary key both find no visible
  version. Without a check against uncommitted versions, both commit and the key has two current
  versions.
- **Delete then reinsert.** A delete writes a tombstone; a later insert of the same key must link
  after the tombstone, not start a second chain, or old snapshots see both rows.
- **The commit race.** Assigning a commit timestamp and stamping the versions are two steps.
  Without one of the fixes above, readers see half a transaction, and the bug appears only under
  concurrency.
- **Long-running transactions.** One forgotten open transaction pins the low watermark, and no
  version newer than its snapshot can be collected. Chains grow and reads slow down. Expose the age
  of the oldest snapshot as a metric.
- **Index entries for invisible versions.** An index may point at a version the reader cannot see,
  for example a row whose indexed column changed. Every index hit must be checked against the
  version's visibility, and must re-check the key, before it is returned.
- **Counter overflow.** PostgreSQL's transaction ids are 32 bits and wrap around, which forces
  periodic "freezing" of old rows. A 64-bit counter avoids this in practice, but in this crate the
  increment must still be a `checked_add`, since clippy denies overflowing arithmetic.
- **Aborted versions left in the chain.** If abort marks versions dead instead of unlinking them,
  every reader must treat them as invisible, and garbage collection must remove them regardless of
  the watermark.

## Testing

- **Serial reference model.** Record every transaction's start timestamp, commit timestamp, reads
  and writes. Replay the committed transactions serially in commit order against a plain `BTreeMap`.
  Each read must return the value the map held after applying every transaction with a commit
  timestamp at or below the reader's start timestamp, plus the reader's own earlier writes.
- **Deterministic interleavings.** Run transactions as explicit step lists (begin, read, write,
  commit) under a seeded random scheduler, so a failing interleaving can be replayed exactly. Check
  the invariants after every step.
- **Anomaly tests.** Write one test per anomaly: dirty read, non-repeatable read and lost update
  must be impossible; write skew must be possible under snapshot isolation, so the test documents
  the isolation level actually provided.
- **Garbage collection.** Before and after a collection pass, every running transaction must read
  the same values for every key. Then end all transactions, collect again, and assert each key has
  one version left.
- **Edge cases:** a transaction that reads its own write, deletes it and reads again; an abort after
  a delete; two inserts of the same key; and a snapshot taken while another transaction is in the
  middle of committing.

## Further reading

- [An Empirical Evaluation of In-Memory Multi-Version Concurrency
  Control](https://www.vldb.org/pvldb/vol10/p781-Wu.pdf), Wu et al., 2017. Version storage, ordering
  and garbage collection compared.
- [The Internals of PostgreSQL, chapter 5: Concurrency
  Control](https://www.interdb.jp/pg/pgsql05.html), Suzuki.
- [High-Performance Concurrency Control Mechanisms for Main-Memory
  Databases](https://www.vldb.org/pvldb/vol5/p298_per-akelarson_vldb2012.pdf), Larson et al., 2011.
  MVCC in SQL Server Hekaton.
