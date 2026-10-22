# Lock table and wait-for graph

> Builds on [hash table](../in-memory-indexes/hash-table.md).

A hash table from lockable resources to their holders and waiters, plus a graph of which transaction
waits for which. A cycle in the graph is a deadlock.

- **Used by:** PostgreSQL's lock manager and deadlock detector, InnoDB.
- **Notes:** two-phase locking on top of it gives serialisable schedules.

## How it works

### Layout

A *lock* here is a logical lock held by a transaction until it commits or aborts, on a resource
such as a table, a page or a row. It is different from a *latch*, the short-lived mutex that
protects the lock table's own memory while it is being changed.

The lock table maps each resource id to an entry with two lists: the *granted group*, the
transactions holding the resource and in which mode, and the *wait queue*, the requests that could
not be granted yet, in arrival order. A second map goes from each transaction to the resources it
holds or waits for, so that commit can release everything without scanning the table.

<figure class="diagram">
{{#include figures/lock-table-and-wait-for-graph-layout.svg}}
<figcaption>Three entries of a lock table. All three transactions hold intention locks on the table <code>accounts</code> before locking its rows. T2 waits for <code>S</code> on row 42, which T1 holds in <code>X</code>, and T1 waits for <code>X</code> on row 77, which T2 and T3 hold in <code>S</code>.</figcaption>
</figure>

Each request has a *mode*. Shared (`S`) allows other readers; exclusive (`X`) allows no one else.
Locking a whole table in `S` or `X` blocks every row, and locking each row separately makes a table
lock expensive to check, so *intention* modes are taken on the table before locking rows inside it:
`IS` ("I will take `S` on some rows"), `IX` ("I will take `X` on some rows"), and `SIX` (`S` on the
table plus `IX`). The standard compatibility matrix, where "yes" means both can be held at once by
different transactions:

```text
            IS    IX    S     SIX   X
     IS     yes   yes   yes   yes   no
     IX     yes   yes   no    no    no
     S      yes   no    yes   no    no
     SIX    yes   no    no    no    no
     X      no    no    no    no    no
```

### Acquire

To request mode `m` on resource `r` for transaction `T`:

1. Find or create the entry for `r`.
2. If `T` already holds a mode on `r` at least as strong as `m`, return at once.
3. If `m` is compatible with every mode granted to other transactions, and no request is already
   waiting in the queue, add `T` to the granted group and return.
4. Otherwise, append the request to the wait queue and block `T` until it is granted or chosen as a
   deadlock victim.

Step 3 refuses to overtake waiters even when `m` is compatible with the holders. Without that rule,
a stream of `S` requests can keep an `X` request waiting forever.

### Release

1. Remove `T`'s mode from the granted group of `r`.
2. Walk the wait queue from the front. Grant each request that is compatible with the granted group
   and with every request granted earlier in this walk, and wake its transaction.
3. Stop at the first request that cannot be granted, so the order of the queue is kept.
4. If the granted group and the queue are both empty, remove the entry.

<figure class="diagram">
{{#include figures/lock-table-and-wait-for-graph-queue.svg}}
<figcaption>Fairness on <code>accounts/77</code>. T4 asks for <code>S</code>, which the holders would allow, but T1's <code>X</code> request is already waiting, so T4 queues behind it. When T2 and T3 release, the walk grants T1 and stops at T4, which conflicts with T1's <code>X</code>.</figcaption>
</figure>

Under *two-phase locking* (2PL), a transaction acquires locks in a growing phase and releases them
in a shrinking phase, and never acquires after it has released. That rule makes the schedule
conflict-serialisable. *Strict* 2PL holds every exclusive lock until commit or abort, so no other
transaction reads data that might still be rolled back; most systems use it, releasing everything at
commit.

### Upgrade

A transaction holding `S` that now wants `X` on the same resource is *upgrading*.

1. If `T` is the only holder, change its mode to `X` at once.
2. Otherwise, queue the upgrade request ahead of ordinary waiters, since `T` already holds the
   resource and every new request behind it would wait for `T` anyway.
3. Grant it when the other holders release.

Two transactions that both hold `S` and both request `X` wait for each other: a *conversion
deadlock*. Some systems offer an update mode (`U`), compatible with `S` but not with another `U`,
for reads that intend to write, so that only one transaction can be on its way to `X`.

### Deadlock detection

The *wait-for graph* has a node per transaction and an edge `A → B` when `A` waits for a lock that
`B` holds in a conflicting mode. PostgreSQL also adds a "soft" edge when `A` is queued behind a
conflicting *request* of `B`, since `A` cannot be woken before `B`.

1. Build the graph from the lock table: for each waiting request, add an edge to each holder whose
   granted mode conflicts with it, and to each conflicting request ahead of it in the queue.
2. Search for a cycle with a depth-first search, tracking the nodes on the current path; an edge
   back to a node on the path closes a cycle.
3. Pick a victim in the cycle, for example the youngest transaction or the one holding the fewest
   locks, and abort it. Its locks are released, which wakes its waiters and breaks the cycle.
4. Repeat until no cycle is left.

<figure class="diagram">
{{#include figures/lock-table-and-wait-for-graph-cycle.svg}}
<figcaption>The wait-for graph of the lock table above. T2 waits for T1 on row 42, and T1 waits for both holders of row 77. The edges between T1 and T2 close a cycle; aborting either one releases its locks and breaks it.</figcaption>
</figure>

Detection does not have to run on every wait. PostgreSQL's waiters sleep for `deadlock_timeout`
(one second by default) and run the detector only if the lock still has not been granted, because
most waits end on their own.

### Variants

- **Deadlock prevention.** Instead of detecting cycles, order transactions by start time and refuse
  some waits. In *wait-die*, an older transaction may wait for a younger one, but a younger one that
  would wait for an older one aborts. In *wound-wait*, an older transaction aborts ("wounds") the
  younger holder, and a younger one waits. Either way no cycle can form. A restarted transaction
  keeps its original timestamp, so it eventually becomes the oldest and cannot be starved.
- **Timeouts.** Abort any transaction that waits longer than a fixed time. It needs no graph but
  aborts transactions that were only slow.
- **Range locks.** Locking only existing rows lets another transaction insert a new row that a
  repeated range scan would see, a *phantom*. Serialisable 2PL needs locks on ranges or on the gaps
  between index keys; InnoDB's next-key locks are an example.

## Invariants

- The modes granted on a resource to different transactions are pairwise compatible.
- A request is in the wait queue only if it conflicts with a granted mode or with a request ahead
  of it.
- The per-transaction map and the per-resource entries agree: `T` is listed on `r` if and only if
  `r` is listed for `T`.
- A transaction waits for at most one request at a time.
- Under 2PL, no transaction acquires a lock after it has released one.
- After deadlock resolution, the wait-for graph has no cycle.

## Implementation pitfalls

- **Holding the latch while blocking.** A transaction that waits for a lock while still holding the
  lock table's mutex blocks every other transaction, including the one it is waiting for. Enqueue
  the request, release the latch, then wait on a per-request condition variable or channel.
- **Lost wake-ups.** If the waiter checks "am I granted?" and then sleeps, a release that happens in
  between wakes nobody. The check and the sleep must be atomic with respect to the release, which
  is what a condition variable used with its mutex provides.
- **Re-entrant requests.** A transaction asking again for a lock it holds must not wait for itself.
  Decide whether a repeated request is counted, and released once or as many times as it was taken.
- **Victims that are not cleaned up.** Aborting a deadlock victim must remove its waiting request
  and release its granted locks, then wake the waiters behind them. A victim whose request stays in
  a queue blocks the queue forever.
- **Reading the graph while it changes.** Building the graph from a lock table that other threads
  are modifying can report cycles that never existed, or miss real ones. PostgreSQL's detector takes
  every partition lock of the lock table for this reason, which is part of why it runs rarely.
- **Choosing the same victim repeatedly.** If a transaction that restarts gets a new identity and
  is again the youngest, it can be chosen as the victim every time. Keep its original start time.
- **Waits the lock table cannot see.** A transaction waiting on a latch, a full buffer pool or a
  network reply while holding locks can deadlock with lock waiters, and no edge in the graph shows
  it. Never wait on anything else while blocking on a lock, or add timeouts as a backstop.
- **Lock escalation surprises.** Systems that convert many row locks into one table lock, to save
  memory, can turn a harmless workload into a conflicting one. If you add escalation, test it with
  concurrent writers to different rows.

## Testing

- **Compatibility table.** A table-driven test over all 25 mode pairs, checking that the second
  request is granted or queued as the matrix says.
- **Brute-force cycle oracle.** For small random wait-for graphs, compare the detector with an
  exhaustive search over all node orderings or with reachability from every node to itself.
- **Classic deadlocks.** Two transactions locking two rows in opposite orders; three transactions
  in a ring; a conversion deadlock between two `S` holders that both upgrade. Each must end with
  exactly one victim and the others completing.
- **Serialisability of random schedules.** Run random transactions through 2PL with a seeded
  scheduler, record the conflicting operations in execution order, build the precedence graph (an
  edge `A → B` when an operation of `A` conflicts with a later one of `B`), and assert it has no
  cycle.
- **Fairness.** Queue an `X` request behind an `S` holder, then send a stream of new `S` requests,
  and assert the `X` request is granted after the first holder releases.

## Further reading

- [Architecture of a Database
  System](https://courses.cs.washington.edu/courses/cse544/20wi/papers/DBArchitecture.pdf),
  Hellerstein, Stonebraker and Hamilton, 2007. Section 6 covers the lock manager.
- [PostgreSQL lock manager
  README](https://github.com/postgres/postgres/blob/master/src/backend/storage/lmgr/README). Lock
  tables, fast-path locks and deadlock detection.
- Gray and Reuter, *Transaction Processing: Concepts and Techniques*, Morgan Kaufmann, 1992.
  Chapters 7 and 8 implement a lock manager.
