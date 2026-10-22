# Copy-on-write B+ tree

> Builds on [B+ tree](../page-based-trees/b-plus-tree.md).

A B+ tree that never changes a page in place. An update copies the leaf and every page on the path
up to the root, then publishes the new root in one atomic write. Readers holding the old root keep a
consistent snapshot without taking locks, and a crash before the root switch leaves the old tree
intact, so no WAL is needed.

- **Used by:** LMDB, Btrfs, CouchDB (which appends the copied pages to its file).
- **Cost:** `O(log_B n)` page writes per update, since the whole path is copied. Old pages can be
  freed only once no reader still holds a root that refers to them.
- **Notes:** Rodeh, 2008 ("B-trees, shadowing, and clones").

## How it works

### Layout

The tree pages are those of a [B+ tree](b-plus-tree.md), with one difference: leaves have **no
sibling links**. Rodeh points out that leaf chaining does not work with shadowing (copying a page
instead of changing it): copying a leaf would change its address, so its left neighbour's link
would have to change too, which means copying the neighbour, its parent, and so on.

The design described here follows LMDB, which keeps the rest of the file small:

- **Two meta pages**, at page 0 and page 1, used alternately. Each holds a transaction ID, the root
  page ID of the data tree, the root page ID of a *free-list tree*, and the highest page number in
  use.
  The valid meta page with the higher transaction ID is the current one.
- **Tree pages**, which are never modified once a transaction that wrote them has committed.
- **A free-list tree**, itself a copy-on-write B+ tree, mapping a transaction ID to the list of
  pages that transaction stopped using.
- **A reader table**, kept outside the data file in shared memory, with one slot per active reader
  holding the transaction ID of the snapshot it is reading.

<figure class="diagram">
{{#include figures/copy-on-write-b-plus-tree-path-copy.svg}}
<figcaption>Transaction 42 changed a key in leaf <code>G</code>. It copied <code>G</code>, <code>C</code> and <code>A</code>, and the copies point at the same pages <code>B</code> and <code>F</code> as before. A reader still on txn 41 sees <code>A</code>, <code>C</code> and <code>G</code>; these grey pages go on the free list under txn 42.</figcaption>
</figure>

### Read transaction

1. Pick the valid meta page with the higher transaction ID, and remember its root.
2. Record that transaction ID in a reader-table slot.
3. Search and scan as in a B+ tree, from that root. Nothing the reader can reach will change.
4. Clear the reader slot when finished.

No latches are needed: a committed page is immutable, and a writer never overwrites a page that a
registered reader might reach.

### Write transaction

There is one writer at a time (LMDB takes a mutex). The writer keeps a map from original page ID to
its *dirty copy* for the current transaction.

1. Start from the current root. Descend to the leaf as in a B+ tree, keeping the path.
2. **Shadow the path:** for each page on the path, from the leaf up, allocate a new page ID, copy
   the page into it, make the change there, and point the parent's copy at the new child ID. A page
   that already has a dirty copy in this transaction is changed in place instead of being copied
   again.
3. Splits and merges work as in a B+ tree, on the copies. A split allocates one more page; a merge
   frees one.
4. Record every replaced or freed page ID in the transaction's list of freed pages.
5. Repeat for further changes in the same transaction. The root of the dirty tree is the new root.

To allocate a page, take one from the free-list tree if it is safe to reuse (see below), and
otherwise extend the file.

### Commit

1. Save the transaction's freed-page list into the free-list tree under the current transaction
   ID. This is itself a copy-on-write update and may allocate and free pages, so it may need to be
   repeated until the list stops changing.
2. Write every dirty page to its new location.
3. `fsync` the file, so every new page is durable.
4. Write the *older* meta page with the new transaction ID, the new data root and the new free-list
   root.
5. `fsync` again. The commit is durable once this returns.

A crash before step 4 completes leaves the newer meta page pointing to the old tree, whose pages
were never overwritten. This is why the scheme needs no write-ahead log. The cost is one copy of
every page on the path per transaction, and two syncs per commit.

### Reusing freed pages

A page freed by transaction `T` is still part of every snapshot older than `T`. It can be reused
once no reader is looking at a snapshot older than `T`:

1. Scan the reader table for the smallest transaction ID still in use. Each reader records the ID
   of the last committed transaction, whose snapshot it reads.
2. Pages freed by transaction `T` are still part of snapshot `T - 1`, but not of snapshot `T`. So
   a free-list entry recorded by `T` can be taken once every reader's ID is at least `T`: its
   pages are then unreachable from every snapshot still in use. Getting this comparison off by one
   in the permissive direction hands a reader's live page to the writer.

A reader that keeps a snapshot open for a long time blocks reuse, and the file grows meanwhile.

### Range scan

Without sibling links, a scan keeps a cursor: a stack of `(page ID, slot index)` pairs from the
root to the current leaf. To move to the next leaf, pop levels until one has a next slot, advance
it, and descend along the leftmost children back to a leaf.

### Variants

- **Append-only:** CouchDB writes every copied page at the end of the file, followed by a new
  header, and never reuses space; a separate compaction pass copies the live data into a new file.
- **Reference-counted clones:** Rodeh's scheme, which Btrfs builds on, keeps a reference count per
  block so that many writable snapshots (clones) can share pages. A page with a count of one can be
  changed in place; one with a higher count is copied, and its children's counts are incremented.
- **Top-down updates:** Rodeh's algorithms split full nodes and fix up sparse ones on the way down,
  so the path never has to be revisited bottom-up, at the cost of looser bounds on node occupancy.

## Invariants

- No page reachable from a committed root is ever written again while any snapshot that can reach
  it is in use.
- The two meta pages hold different transaction IDs, and the newer valid one points to a fully
  written, fully synced tree.
- Every page in the file is in exactly one of three states: reachable from the current root, on the
  free list, or reachable only from snapshots still being read. No page is lost from all three
  (a leak), and none is in two (a double use).
- A free-list entry recorded by transaction `T` is only reused once every active reader's
  transaction ID is at least `T`.
- Each committed version, read through its own root, satisfies every
  [B+ tree](b-plus-tree.md) invariant.

## Implementation pitfalls

- **Syncing in the wrong order.** If the meta page is written or synced before the tree pages are
  durable, a crash can leave a meta page pointing at pages that were never written. Sync the pages,
  then write and sync the meta page.
- **Torn meta pages.** A crash can leave the meta page being written half old and half new. Give
  each meta page a checksum, and on open fall back to the other one if it does not verify.
- **Reusing pages too early.** The off-by-one between "freed by transaction `T`" and "visible to a
  reader of snapshot `T - 1`" is easy to get wrong. A reader of `T - 1` can still reach pages that
  `T` freed. Test it directly.
- **Changing the free list while saving it.** Saving the freed pages into the free-list tree
  allocates and frees pages of the free-list tree itself. Loop until no new page is allocated or
  freed, and during that loop do not take pages from the free list that the loop itself added.
- **Copying a page twice in one transaction.** Without the dirty-page map, every change copies the
  whole path again, and earlier copies leak. Keep the map, and change dirty pages in place.
- **Memory maps need `unsafe`.** LMDB reads the file through `mmap`. In Rust, mapping a file is an
  `unsafe` operation (another process can change the bytes under a `&[u8]`), and this crate forbids
  `unsafe`. Read pages with positioned reads through the
  [buffer pool](../storage-primitives/buffer-pool.md) instead. A committed page can then be shared
  between readers as an immutable `Arc<[u8]>`.
- **Parent pointers and page IDs.** A copied page gets a new page ID, so anything that stored the
  old ID must also be copied. That is why the whole path is copied, and why parent pointers and
  sibling links cannot be stored in pages.
- **Crashed readers.** A process that dies with a reader slot set blocks page reuse forever. LMDB
  records the process and thread ID in each slot to detect stale entries.

## Testing

- **Model test with snapshots.** Keep a `BTreeMap` per committed transaction (cloning is fine in a
  test). Open readers at random points, keep them while later transactions commit, and check each
  reader always sees exactly its snapshot.
- **Page accounting.** After each commit, walk every live root and the free list, and check that
  every page in the file is accounted for once, as in the invariants.
- **Crash simulation.** Use a byte store that buffers writes and, on a simulated crash, keeps only
  what was synced (and, to be thorough, a random subset of unsynced pages). Reopen and check the
  contents equal either the last committed state or the one before it, never a mix.
- **Torn meta write.** Corrupt one byte of the newer meta page and check the open falls back to the
  older one.
- **Long-lived readers.** Hold one reader open across many write transactions and check that no
  page it can reach is reused, then close it and check that the freed pages are reused and the file
  stops growing.
- **Abort.** Abandon a write transaction halfway and check that nothing it allocated leaks and that
  the next transaction sees the previous commit.

## Further reading

- [B-trees, Shadowing, and Clones](https://www.usenix.org/legacy/events/lsf07/tech/rodeh.pdf),
  Rodeh.
- [MDB: A Memory-Mapped Database and Backend for
  OpenLDAP](http://www.lmdb.tech/media/20120829-LinuxCon-MDB-txt.pdf), Chu, 2012. The design of
  LMDB.
- [Build Your Own Database From Scratch](https://build-your-own.org/database/), Ye. Its B+ tree is
  copy-on-write, with a free list for page reuse.
- [Btrfs B-trees](https://btrfs.readthedocs.io/en/latest/dev/dev-btrees.html), Btrfs developer
  documentation.
