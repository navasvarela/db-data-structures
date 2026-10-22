# Logical clocks

Timestamps that order events across machines without trusting wall clocks: Lamport clocks give a
total order consistent with causality, vector clocks detect concurrent updates, and hybrid logical
clocks stay close to physical time.

- **Used by:** vector clocks in Dynamo and Riak; hybrid logical clocks in CockroachDB and
  YugabyteDB.
- **Notes:** Lamport, 1978; Kulkarni et al., 2014 (hybrid logical clocks).

## How it works

### Happens-before

Lamport defines the order the clocks are meant to capture. Event `e` *happens before* event `f`,
written `e → f`, if `e` comes before `f` on the same process, or `e` is the sending of a message
and `f` its receipt, or there is a chain of such steps from `e` to `f`. Two events where neither
happens before the other are *concurrent*. Each clock below stamps events so that the order of
stamps says something about `→`.

<figure class="diagram">
{{#include figures/logical-clocks-happens-before.svg}}
<figcaption>Three processes exchanging two messages. The highlighted path gives <code>a1 → a2 → b2 → b3 → c2</code>. No path connects <code>a3</code> and <code>b3</code>, or <code>c1</code> and <code>a1</code>, so those pairs are concurrent.</figcaption>
</figure>

### Lamport clocks

Each process keeps one integer counter `C`, starting at 0.

1. **Local event or send:** `C = C + 1`, and stamp the event (and the outgoing message) with `C`.
2. **Receive a message stamped `t`:** `C = max(C, t) + 1`, and stamp the receive event with `C`.
3. **Compare:** order events by `(C, process id)`. The process id breaks ties, which gives a total
   order of all events.

This gives one direction only: if `e → f` then `C(e) < C(f)`. The converse does not hold: a smaller
stamp does not mean the event happened before, since concurrent events are ordered arbitrarily.

### Vector clocks

Each process `i` keeps a vector `V` with one counter per process, all 0 at first. `V[j]` is the
number of events of process `j` that process `i` knows about.

1. **Local event or send:** `V[i] = V[i] + 1`, and stamp the event (and the message) with a copy of
   `V`.
2. **Receive a message stamped `W`:** set `V[j] = max(V[j], W[j])` for every `j`, then
   `V[i] = V[i] + 1`.
3. **Compare** two stamps `X` and `Y`:
   - `X ≤ Y` if `X[j] ≤ Y[j]` for every `j`;
   - `X < Y`, meaning the event stamped `X` happened before the one stamped `Y`, if `X ≤ Y` and
     `X ≠ Y`;
   - otherwise, if neither `X ≤ Y` nor `Y ≤ X`, the events are concurrent.

Vector clocks capture `→` exactly: `e → f` if and only if `V(e) < V(f)`. The price is a counter per
process in every stamp.

<figure class="diagram">
{{#include figures/logical-clocks-stamps.svg}}
<figcaption>The same run with both clocks. Lamport stamps increase along every path, but they also order <code>a3</code> (3) before <code>b3</code> (4), which are concurrent. Their vectors, <code>[3,0,0]</code> and <code>[2,3,0]</code>, are each larger in one entry, so neither is below the other and the concurrency shows.</figcaption>
</figure>

A related structure, the *version vector*, stamps versions of one data item rather than events: a
replica increments its own entry only when it updates the item. Dynamo, which calls them vector
clocks, stores one with each object; when a read returns versions whose vectors are concurrent, both
are kept and the conflict is handed to the application to reconcile.

### Hybrid logical clocks

A hybrid logical clock (HLC) stamps events with a pair `(l, c)`. `l` is the largest physical time
the process has heard of, from its own clock `pt` or from any message, and `c` is a counter that
orders events sharing the same `l`. The algorithm of Kulkarni et al. (2014):

```text
send or local event:
    l_old = l
    l = max(l_old, pt)
    c = if l == l_old { c + 1 } else { 0 }
    stamp (l, c)

receive a message stamped (l_m, c_m):
    l_old = l
    l = max(l_old, l_m, pt)
    c = if l == l_old && l == l_m { max(c, c_m) + 1 }
        else if l == l_old        { c + 1 }
        else if l == l_m          { c_m + 1 }
        else                      { 0 }
    stamp (l, c)
```

Stamps are compared as pairs, `l` first, then `c`. Like a Lamport clock, `e → f` implies
`(l, c)(e) < (l, c)(f)`. Unlike one, `l` is always at least the local physical time and, the paper
proves, never more than the clock synchronisation error `ε` above it. That lets a database use HLC
stamps as commit timestamps that are also meaningful as wall-clock times, for example to read a
snapshot "as of" a time.

<figure class="diagram">
{{#include figures/logical-clocks-hlc.svg}}
<figcaption>HLC stamps when P's clock runs ahead of Q's. Q receives P's <code>(10, 0)</code> while its own clock reads 8, so <code>l</code> becomes 10 and <code>c</code> becomes 1. Its next event, at physical time 9, keeps <code>l = 10</code> and increments <code>c</code>; once its clock reaches 11, <code>l</code> follows it and <code>c</code> resets to 0.</figcaption>
</figure>

The paper also proves `c` is bounded: `c ≤ N · (ε + 1)` for `N` processes, with `ε` in clock ticks,
and smaller under extra assumptions about message delay. It proposes packing a stamp into 64 bits,
with 48 bits for `l` and 16 for `c`.

### Variants

- **Dotted version vectors** and other refinements of version vectors fix the cases where plain
  version vectors, updated by servers on behalf of many clients, either grow or report false
  conflicts.
- **Truncated vectors.** Dynamo bounds the size of its version vectors by storing a timestamp with
  each entry and dropping the oldest entry once the vector reaches a threshold (the paper says "say
  10"). It accepts that some descendant relationships can then no longer be recognised.

## Invariants

- **Lamport:** a process's counter strictly increases with each event, and `e → f` implies
  `C(e) < C(f)`.
- **Vector:** `V[i]` of process `i` equals the number of events process `i` has had; no entry ever
  decreases; and `e → f` if and only if `V(e) < V(f)`.
- **HLC:** for every event, `l ≥ pt`; `l` never decreases on a process; `e → f` implies
  `(l, c)(e) < (l, c)(f)`; and, while physical clocks stay within `ε` of each other,
  `l - pt ≤ ε`.

## Implementation pitfalls

- **Reading too much into Lamport stamps.** `C(e) < C(f)` does not mean `e` caused `f`. Code that
  uses Lamport stamps to detect conflicts will silently drop concurrent updates; that needs vector
  clocks or version vectors.
- **Missing vector entries.** Vectors are usually stored as maps from process id to counter, and a
  missing entry means 0. Comparison must walk the union of both maps' keys; comparing only the keys
  of one map reports `X ≤ Y` when `X` has an entry `Y` lacks.
- **Vectors that only grow.** Every process that ever wrote stays in the vector. With many
  short-lived clients as writers, vectors become large, which is why systems stamp with server ids
  and why Dynamo truncates. Truncation loses information, so it must be a deliberate choice.
- **A physical clock that goes backwards.** NTP can step the system clock back. HLC tolerates this,
  since `l` never decreases and `c` takes over, but code that reads `pt` and assumes it increases
  will not. In Rust, `SystemTime` can go backwards and `Instant` cannot, but `Instant` is not
  comparable across machines; HLC needs the former and must handle its jumps.
- **Messages from the future.** A message stamped with an `l` far ahead of local time drags `l`
  forward on every process it reaches, and they all stay ahead until physical time catches up. One
  remedy is to refuse messages whose `l` is ahead of the local `pt` by more than a configured
  bound, and to log and report them. Pick the bound well above the expected clock error.
- **Counter overflow.** With 16 bits, `c` overflows at 65,536 events sharing one `l`. The paper
  found 16 bits ample in its experiments, but a stuck physical clock makes overflow possible.
  In this crate, clippy denies overflowing arithmetic, so the increment must be `checked_add`, and
  the overflow case needs a decision: wait for `pt` to advance, or return an error.
- **The range of 48 bits.** The paper derives `l` from the top 48 bits of a 64-bit NTP timestamp,
  which keeps whole seconds and about 15 microseconds of fraction. If you instead store Unix time in
  microseconds, 48 bits last only about 8.9 years from your epoch; in milliseconds they last about
  8,900 years. Choose the unit and epoch on purpose, and round `pt` the same way everywhere.
- **Ties between processes.** Two processes can produce the same `(l, c)`. If stamps must be unique,
  for example as commit timestamps, append the process id as a final tie-breaker.

## Testing

- **Reference model.** In a simulation, record every event and every message, and compute the true
  happens-before relation from that graph by reachability. Check each clock's guarantee against it:
  Lamport and HLC stamps must increase along every `→` edge, and vector stamps must satisfy
  `V(e) < V(f)` exactly when `e → f`.
- **Random executions.** Generate runs with several processes, random local events, random message
  sends, and random delivery order and delay, from a seeded generator.
- **Skewed clocks for HLC.** Give each simulated process a physical clock with its own offset within
  `±ε/2`, and assert `l ≥ pt` and `l - pt ≤ ε` after every event, and that `c` stays within the
  paper's bound.
- **Edge cases:** a physical clock that jumps backwards, one that stops, a message from far in the
  future (it must be rejected when outside the bound), a counter at its maximum, and comparison of
  vectors with disjoint sets of process ids.

## Further reading

- [Time, Clocks, and the Ordering of Events in a Distributed
  System](https://lamport.azurewebsites.net/pubs/time-clocks.pdf), Lamport, 1978.
- [Logical Physical Clocks and Consistent Snapshots in Globally Distributed
  Databases](https://cse.buffalo.edu/tech-reports/2014-04.pdf), Kulkarni et al., 2014. Hybrid
  logical clocks.
