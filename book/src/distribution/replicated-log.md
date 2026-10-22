# Replicated log

> Builds on [append-only log](../storage-primitives/append-only-log.md).

An ordered log of commands that a consensus protocol keeps identical across replicas. Each replica
applies the log to its own state machine.

- **Used by:** Raft in etcd, CockroachDB and TiKV; Kafka's partition replication.
- **Notes:** Ongaro and Ousterhout, 2014 (Raft).

## How it works

This page follows Raft, as specified in figure 2 of the paper. Read that figure next to this page:
almost every rule in it exists to prevent a specific failure, and the pitfalls below are mostly
cases of leaving one out.

### Layout

A cluster has a fixed set of servers, usually 3 or 5. Time is divided into *terms*, numbered
1, 2, 3 and so on; each term starts with an election and has at most one leader. Every server is in
one of three roles: *follower*, *candidate* or *leader*.

Each server keeps a log of entries. An entry holds a command for the state machine and the term in
which the leader received it. Log indices start at 1. An entry is *committed* once the leader knows
it is stored on a majority of servers; committed entries are never lost and are applied to every
server's state machine in index order.

<figure class="diagram">
{{#include figures/replicated-log-layout.svg}}
<figcaption>Five servers in term 3, with each entry's term in its cell. Entry 5 is on S1, S2 and S3, a majority, so the commit index is 5. Entry 6 is on two servers only. S5's entries 4 and 5 came from an old term-2 leader, were never committed, and will be overwritten.</figcaption>
</figure>

The state each server keeps, from figure 2:

- **Persistent**, written to stable storage before answering any RPC: `currentTerm`, the latest term
  the server has seen; `votedFor`, the candidate it voted for in that term, if any; and the log.
- **Volatile:** `commitIndex`, the highest index known to be committed, and `lastApplied`, the
  highest index applied to the state machine.
- **Leader only**, reset after each election: for each follower, `nextIndex`, the next entry to
  send it, and `matchIndex`, the highest entry known to be replicated on it.

Servers talk through two RPCs: `RequestVote`, sent by candidates, and `AppendEntries`, sent by the
leader to replicate entries and, with no entries, as a heartbeat.

### Rules every server follows

1. If any RPC request or response carries a term `T > currentTerm`, set `currentTerm = T`, clear
   `votedFor`, and become a follower. Then continue handling the message.
2. If a request carries a term lower than `currentTerm`, reject it and include `currentTerm` in the
   reply, so the sender learns it is stale.
3. While `commitIndex > lastApplied`, increment `lastApplied` and apply that entry to the state
   machine.

### Leader election

A follower expects regular `AppendEntries` from the leader. Each server picks an *election timeout*
at random from a fixed interval (the paper uses 150–300 ms as an example); randomness makes it
unlikely that two servers time out together and split the vote.

1. A follower whose election timeout passes without hearing from the current leader, or granting a
   vote, becomes a candidate.
2. The candidate increments `currentTerm`, votes for itself, persists both, resets its election
   timer and sends `RequestVote(term, lastLogIndex, lastLogTerm)` to every other server.
3. A receiver grants its vote if the request's term is not stale, it has not voted for anyone else
   in this term, and the candidate's log is *at least as up to date* as its own: the candidate's
   last term is higher, or the last terms are equal and the candidate's log is at least as long.
   It persists `votedFor` before replying, and resets its own election timer when it grants.
4. A candidate that receives votes from a majority, counting itself, becomes leader and at once
   sends heartbeats to every server.
5. A candidate that receives an `AppendEntries` from a leader with a term at least its own becomes a
   follower.
6. A candidate whose election timeout passes without a result starts a new election in a new term.

<figure class="diagram">
{{#include figures/replicated-log-roles.svg}}
<figcaption>The three roles and the events that move a server between them. Any server, in any role, that sees a higher term in a message becomes a follower.</figcaption>
</figure>

The up-to-date check in step 3 is what makes a newly elected leader hold every committed entry:
a committed entry is on a majority, and the winner needed votes from a majority, so at least one
voter had the entry and would only vote for a log at least as up to date.

### Appending and replicating

1. The leader receives a command from a client, appends it to its own log with `currentTerm`, and
   persists it.
2. For each follower, the leader sends `AppendEntries` carrying the entries from `nextIndex`
   onwards, the index and term of the entry immediately before them (`prevLogIndex`, `prevLogTerm`),
   and the leader's `commitIndex` (`leaderCommit`).
3. The follower handles it in this order:
   1. Reply false if the term is stale.
   2. Reply false if its log has no entry at `prevLogIndex` with term `prevLogTerm`. This is the
      *consistency check*.
   3. For each entry in the request: if an existing entry at that index has a different term, delete
      it and every entry after it. If no entry exists at that index, append the remaining entries.
      Entries that already match are left alone.
   4. Persist the log, then set `commitIndex = min(leaderCommit, index of the last new entry)` if
      `leaderCommit` is greater than its `commitIndex`.
   5. Reply true.
4. On success, the leader sets that follower's `matchIndex` to `prevLogIndex + number of entries
   sent` and `nextIndex` to one more. On a consistency-check failure, it lowers `nextIndex` and
   retries.

Because every `AppendEntries` checks the entry before the new ones, induction gives the *Log
Matching* property: if two logs have an entry with the same index and term, they are identical up
to that index.

### Committing

1. The leader looks for the highest `N > commitIndex` such that a majority of `matchIndex` values
   (counting itself) are at least `N`, and the leader's entry at `N` has term `currentTerm`.
2. If it finds one, it sets `commitIndex = N`. Every earlier entry is committed with it.
3. Followers learn the new `commitIndex` from `leaderCommit` in the next `AppendEntries`.

The leader never commits an entry from an earlier term by counting replicas. Figure 8 of the paper
shows an entry from an old term that reaches a majority, is then overwritten by another leader, and
would have been lost had it been counted as committed. Old entries become committed indirectly, when
an entry of the current term after them is committed. For the same reason, a new leader appends a
blank *no-op* entry at the start of its term, so that it can commit something promptly.

### Repairing a follower's log

When a follower's log has diverged, as S4 and S5 have above, the consistency check fails and the
leader moves `nextIndex` back until it reaches an index where the two logs agree. From there,
step 3.3 of the follower's handling deletes the conflicting suffix and appends the leader's entries.

Moving back one entry per round trip is slow for a far-behind follower. The Students' Guide gives a
faster version:

1. On a failed check, the follower returns a `conflictIndex` and `conflictTerm`. If its log is too
   short, `conflictIndex` is its last index plus one and `conflictTerm` is empty. Otherwise,
   `conflictTerm` is the term of its entry at `prevLogIndex`, and `conflictIndex` is the first index
   holding an entry of that term.
2. The leader searches its log for `conflictTerm`. If it has entries of that term, it sets
   `nextIndex` to one past the last of them; otherwise, it sets `nextIndex = conflictIndex`.

The guide writes the "log too short" case as `conflictIndex = len(log)`. What matters is that it
names the first index the follower is missing, which with the paper's 1-based indices is the
follower's last index plus one, as in step 1.

<figure class="diagram">
{{#include figures/replicated-log-repair.svg}}
<figcaption>The leader of the first figure repairs S5 with the faster scheme. S5 has no entry 6, so the next try starts at 6. S5's entry 5 has term 2, which it first holds at index 3; the leader's last term-2 entry is also 3, so it sets <code>nextIndex</code> to 4. The check at index 3 passes, and S5 replaces its conflicting entries 4 and 5 with the leader's 4 to 6.</figcaption>
</figure>

### Persisting the log on this crate's append-only log

The crate's [append-only log](../storage-primitives/append-only-log.md) is a good base for the Raft
log, with three additions:

1. **Entry records.** Encode each entry as `(index, term, command)` in one record, and keep an
   in-memory vector from log index to file offset, rebuilt by a scan on open.
2. **Suffix truncation.** Raft must delete conflicting entries from the end. Truncating the file to
   the offset of the first deleted entry does this, so the byte store needs a truncate operation
   that also resets its end offset.
3. **Durability.** Raft's persistence rule means a follower must `fsync` its log before replying,
   and the leader before counting itself towards a majority. A torn record at the tail after a crash
   must be detected, with the per-record checksums of a [write-ahead
   log](../storage-primitives/write-ahead-log.md), and cut off.

`currentTerm` and `votedFor` change rarely and must never go backwards, so they usually live in a
separate small file, rewritten by writing a temporary file, syncing it and renaming it over the old
one.

### Client requests and reads

1. A client sends each command to the leader; a follower that receives one redirects the client.
2. The leader replies after the entry is committed and applied.
3. Clients retry when a reply is lost, so a command can be appended twice. Give each client an id
   and each command a sequence number, and have the *state machine* remember the last sequence
   number applied per client, so every replica ignores the same duplicates.
4. A read served from the leader's state without going through the log can be stale if a newer
   leader has been elected. The paper's fix: the leader must have committed an entry in its current
   term, and must exchange heartbeats with a majority after receiving the read and before answering
   it.

### Variants

- **Log compaction.** The log cannot grow forever. Each server periodically snapshots its state
  machine, records the index and term of the last entry the snapshot covers, and discards the log up
  to it. A leader sends `InstallSnapshot` to a follower that needs entries it has already
  discarded.
- **Membership changes.** Adding or removing servers safely needs care, because a direct switch
  can let the old and new configurations elect two leaders in the same term. The paper uses a
  transitional *joint consensus* configuration, in which decisions need majorities of both the old
  and the new set.
- **Pre-vote.** Described in Ongaro's dissertation: before incrementing its term, a candidate asks
  whether it could win. A server that was partitioned away then cannot disrupt a working leader by
  returning with a higher term.
- **Kafka.** Kafka's partition replication is also leader-based, but a partition's leader is chosen
  by a cluster controller from the replicas known to be in sync, rather than by an election among
  the replicas themselves.

## Invariants

The five safety properties of figure 3 of the paper, each of which can be checked by a test that
records every server's state over time:

- **Election Safety:** at most one leader is elected in a given term.
- **Leader Append-Only:** a leader never overwrites or deletes entries in its log; it only appends.
- **Log Matching:** if two logs contain an entry with the same index and term, the logs are
  identical in all entries up to that index.
- **Leader Completeness:** if an entry is committed in a given term, it is present in the logs of
  the leaders of all higher terms.
- **State Machine Safety:** if a server has applied the entry at a given index, no other server ever
  applies a different entry at that index.

Further properties of a single server that make useful assertions:

- `currentTerm` never decreases, also across restarts.
- A server votes for at most one candidate per term, also across restarts.
- `lastApplied <= commitIndex <= last log index`, and `commitIndex` never decreases.
- On the leader, every `matchIndex` is at most its last log index, and never decreases within a
  term.

## Implementation pitfalls

Most of these come from the Students' Guide to Raft, which is worth reading in full.

- **Resetting the election timer too often.** Reset it only when receiving `AppendEntries` from the
  current leader, when starting an election, and when granting a vote. Resetting it on every
  `RequestVote` lets a server with an out-of-date log, which can never win, keep postponing the
  elections that up-to-date servers would win.
- **Treating heartbeats as special.** An empty `AppendEntries` must still pass the consistency
  check. Replying true without it tells the leader the follower's log matches up to `prevLogIndex`,
  which may be false.
- **Truncating on every `AppendEntries`.** Delete entries only on a real conflict (same index,
  different term). A delayed, older `AppendEntries` that carries a prefix of what the follower
  already has must not delete the entries after it, which the follower may already have
  acknowledged.
- **Setting `commitIndex` from the log length.** A follower must use the index of the last *new*
  entry from this request, not the end of its log. Entries beyond it have not been checked against
  the leader's log and may be wrong.
- **Committing old-term entries by counting.** The `log[N].term == currentTerm` condition in the
  commit rule is easy to leave out, and the bug only shows in the scenario of figure 8.
- **Term confusion in replies.** A reply may arrive after the leader has moved to a new term. Check
  that `currentTerm` still equals the term the request was sent in; if not, ignore the reply. Also
  step down if the reply carries a higher term, before anything else.
- **Using current state to process a reply.** Set `matchIndex` from the arguments of the request
  that succeeded (`prevLogIndex + entries sent`), not from `nextIndex - 1` or the leader's log
  length, which may have changed while the request was in flight.
- **Comparing log lengths instead of up-to-dateness.** The vote check compares last terms first,
  then lengths. A longer log with an older last term is less up to date.
- **Persisting after replying.** `currentTerm`, `votedFor` and the log must be durable before any
  reply that depends on them. A server that votes, crashes before persisting, and restarts can vote
  again in the same term for someone else, and two leaders are elected.
- **Applying in the wrong place.** Only one thread should apply entries, in order. Check for newly
  committed entries whenever `commitIndex` changes, not only when sending heartbeats, or an entry
  waits for the next unrelated event to be applied.
- **The same index for two commands.** A command accepted at index `i` may be overwritten by another
  leader's entry at `i`. A client handler waiting on index `i` must check that the entry applied
  there is its own command, and fail otherwise.
- **Indexing from 1 under this crate's lints.** The paper's log starts at index 1, a vector at 0,
  and `prevLogIndex` can be 0 (no previous entry). Clippy denies `log[i]` and subtraction that can
  overflow, so convert with `checked_sub` and read with `get`, and treat "index 0" as an explicit
  case rather than an off-by-one.
- **Hidden time and randomness.** Reading the wall clock or a global random generator inside the
  protocol code makes failures impossible to reproduce. Pass time and the random source in, so the
  simulation below can control them.

## Testing

- **Deterministic simulation.** Run the whole cluster in one thread: servers are state machines
  that take a message or a clock tick and return messages to send. A simulated network, driven by a
  seeded random generator, delays, drops, duplicates and reorders messages, and partitions and heals
  the cluster; a simulated disk loses unsynced writes when a server crashes. A failing seed replays
  the same run exactly.
- **Check the invariants after every step.** Record, across the whole run, which server led each
  term (Election Safety), every committed entry and every leader's log (Leader Completeness), and
  every server's applied sequence (State Machine Safety: each applied sequence is a prefix of the
  longest one). Check Log Matching across every pair of logs.
- **Liveness, separately.** Safety must hold under any schedule. Progress only has to hold once the
  network is healthy: after healing every partition, assert that a leader is elected and new
  commands commit within a bounded number of simulated ticks.
- **Scenario tests.** Reproduce figure 7 (followers with missing and extra entries) and figure 8 (an
  old-term entry on a majority) of the paper as fixed scripts, and assert the outcomes the paper
  describes.
- **Client-level checks.** Record each client operation's start time, end time and result against a
  replicated key-value store, and check that the history is linearisable, either with a checker
  library or, for short histories, by trying every ordering consistent with real time.
- **Crash and restart.** Crash servers at random points, including between appending and syncing,
  and between voting and replying, and check that restarted servers never violate the vote and term
  invariants.

## Further reading

- [In Search of an Understandable Consensus Algorithm](https://raft.github.io/raft.pdf), Ongaro and
  Ousterhout, 2014. The Raft paper; figure 2 is the whole protocol on one page.
- [Students' Guide to Raft](https://thesquareplanet.com/blog/students-guide-to-raft/), Gjengset,
  2016. The mistakes most first implementations make.
- [toyDB](https://github.com/erikgrinaker/toydb), Grinaker. A Raft-replicated key-value store with
  SQL on top, written in Rust to be read.
