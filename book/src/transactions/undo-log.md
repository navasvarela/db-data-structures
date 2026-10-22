# Undo log

> Builds on [write-ahead log](../storage-primitives/write-ahead-log.md).

Records the before-image of each change so that an aborted transaction can be rolled back and older
snapshots can be rebuilt.

- **Used by:** InnoDB rollback segments, Oracle undo tablespaces.

## How it works

### Layout

An undo record describes how to reverse one change. It holds:

- the transaction that made the change;
- the kind of change: insert, update or delete;
- where it happened: the table and the row's key or physical address;
- the *before-image*: for an update, the old values of the changed columns; for a delete, the whole
  old row; for an insert, nothing beyond the key, since undoing it means deleting the row;
- a pointer to the same transaction's previous undo record.

The transaction table keeps, for each active transaction, a pointer to its newest undo record, so
the records of one transaction form a chain from newest to oldest.

<figure class="diagram">
{{#include figures/undo-log-layout.svg}}
<figcaption>The undo chain of transaction T7, which inserted <code>orders/12</code>, deleted <code>orders/9</code> and updated <code>accounts/42</code>, in that order. Rollback starts at the transaction table's pointer and follows the chain.</figcaption>
</figure>

Two placements are common. In ARIES, the undo information sits inside the
[write-ahead log](../storage-primitives/write-ahead-log.md) record of each change, and every record
carries a `PrevLSN`, the log sequence number (LSN) of the same transaction's previous record. In
InnoDB, undo records live in separate undo pages, which are themselves protected by the redo log;
each row stores a pointer to its newest undo record, so the same records also serve
[multi-version reads](mvcc-version-chain.md).

### Recording a change

1. Build the undo record from the row as it is now, before changing it.
2. Append it to the log, or to the transaction's undo pages, and point the transaction's "last undo"
   at it.
3. Change the row. The undo record must reach stable storage no later than the changed page does
   (see the pitfalls).

### Rollback of one transaction

1. Start from the transaction's last undo record.
2. Apply its inverse: delete the inserted row, restore the before-image of an updated row, re-insert
   the deleted row.
3. Write a *compensation log record* (CLR) describing that action. A CLR carries an
   `UndoNxtLSN`: the `PrevLSN` of the record that was undone, which is the next record still left
   to undo.
4. Follow the chain to the previous record and repeat, until the chain ends.
5. Write an "end" record for the transaction and release its locks.

A *partial rollback* to a savepoint runs the same loop, but stops at the record that was the
newest when the savepoint was taken.

### Rollback after a crash

ARIES recovery runs three passes over the log:

1. **Analysis:** scan forward from the last checkpoint and rebuild the table of transactions that
   were active at the crash (the *losers*), each with its last LSN.
2. **Redo:** repeat history. Reapply every logged change whose effect is not already on its page
   (the page stores the LSN of the last change applied to it), including the changes of losers and
   including CLRs, so the database is exactly as it was at the crash.
3. **Undo:** roll back all losers together, always undoing the record with the highest LSN next.
   For an ordinary record, undo it and write a CLR. For a CLR, undo nothing and jump to its
   `UndoNxtLSN`.

If the system crashes again during the undo pass, the next recovery redoes the CLRs already written
and, by following their `UndoNxtLSN`, skips straight past the work they describe. Each change is
therefore undone at most once, however many times recovery restarts.

<figure class="diagram">
{{#include figures/undo-log-rollback.svg}}
<figcaption>T7's log records, assuming its three changes got LSNs 10, 20 and 30. Rolling back writes one CLR per undone record, and each CLR's <code>UndoNxtLSN</code> names the record still to undo. After a crash at the dashed line, recovery redoes CLR 40, and its undo pass follows that CLR to LSN 20, so U3 is not undone a second time.</figcaption>
</figure>

### Serving old versions

When undo records double as version storage, a reader whose snapshot predates a row's newest
version follows the row's undo pointer and applies before-images, newest first, until it reaches a
version its snapshot can see. This is why InnoDB distinguishes two kinds of undo: according to its
documentation, insert undo is needed only for rollback and can be discarded at commit, while update
undo must be kept until no snapshot could still need the older row version. A *purge* process then
removes it, along with rows that were only delete-marked.

### Variants

- **Physical undo:** the before-image is the exact bytes of a page region. Simple, but only valid if
  nothing else has rearranged the page since.
- **Logical undo:** the record says "delete key 12 from index I" instead of "restore these bytes".
  It stays correct after other transactions have split or compacted the page, at the cost of
  running the full index operation during rollback. ARIES supports logical undo for this reason.
- **No undo on disk:** a *no-steal* buffer policy never writes uncommitted changes to disk, so
  crash recovery has nothing to undo. Aborts still need before-images, but only in memory. It
  requires keeping every page a running transaction has dirtied in memory until it commits.

## Invariants

- An undo record is written before the change it reverses, and it reaches stable storage no later
  than the changed page does.
- A transaction's chain is ordered by strictly decreasing LSN.
- After a full rollback, every row the transaction touched is exactly as it was before the
  transaction began, as far as that transaction's changes are concerned.
- A CLR is never undone; each ordinary record is undone at most once.
- An update undo record is not discarded while any active snapshot could need it.

## Implementation pitfalls

- **Undoing an undo.** If rollback actions are logged as ordinary updates, a crash during rollback
  makes recovery undo the rollback itself, restoring the aborted transaction's changes, and then try
  again. CLRs, which are redo-only and point past the work they compensate, are the fix.
- **Writing the change before its undo.** Under a *steal* policy, where the buffer pool may write a
  page with uncommitted changes to disk, a crash between the page write and the undo record's
  write leaves a change that cannot be reversed. The undo record must be durable first, which is the
  write-ahead rule applied to undo.
- **Releasing locks before rollback finishes.** Rollback restores before-images. If another
  transaction has modified the same row in the meantime, restoring the image erases its change.
  Locks must be held until the rollback is complete, which strict two-phase locking guarantees (see
  [lock table](lock-table-and-wait-for-graph.md)).
- **Physical undo after a page split.** If a row moved to another page because a different
  transaction split the page, restoring bytes at the old position corrupts the page. Either use
  logical undo for index changes, or keep pages from being reorganised under uncommitted changes.
- **Order of application.** Undo must run newest first. Undoing an insert before the update that
  followed it, for example, tries to update a row that no longer exists.
- **Large before-images.** Updating one byte of a large value should not copy the whole value into
  undo. Store only the changed columns, and for very large values consider storing them out of line
  and versioning the reference.
- **Long-running transactions.** InnoDB's documentation warns that transactions which stay open,
  including read-only ones, prevent update undo from being discarded and let the undo space grow.
  Expose the age of the oldest snapshot so it can be watched.
- **Unique keys during rollback.** Re-inserting a deleted row can collide with a row another
  transaction inserted under the same unique key after the delete. With correct locking this cannot
  happen; if it does, it is a locking bug, and rollback should fail loudly rather than skip the
  record.

## Testing

- **Reference model.** Copy the full state before a transaction, run a random sequence of inserts,
  updates and deletes, roll back, and assert the state equals the copy.
- **Savepoints.** Take savepoints at random points, roll back to a random one, and compare with a
  copy taken at that savepoint.
- **Crash at every point.** For a transaction that aborts, inject a crash after every log write
  during normal work, during rollback and during recovery's undo pass; recover each time and assert
  the transaction's effects are gone and no committed transaction's effects are.
- **Recovery twice.** Crash in the middle of the undo pass, recover, crash again, recover again.
  The result must equal a single clean recovery, and the number of CLRs must not exceed the number
  of records undone.
- **Edge cases:** a transaction with no changes, one that inserts and then deletes the same row, one
  that updates the same row many times, and a rollback of an insert into a page that was
  reorganised afterwards.

## Further reading

- [ARIES](https://web.stanford.edu/class/cs345d-01/rl/aries.pdf), Mohan et al., 1992. Undo, redo and
  compensation log records.
- [InnoDB undo logs](https://dev.mysql.com/doc/refman/8.4/en/innodb-undo-logs.html), MySQL
  documentation.
