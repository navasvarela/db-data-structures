# Ring buffer

A fixed-capacity circular array with read and write cursors. It bounds memory for data that is
produced and consumed in order.

- **Used by:** WAL buffers before they are flushed, replication queues, LMAX Disruptor-style
  pipelines.
- **Cost:** `O(1)` push and pop with no allocation after creation.

## How it works

### Layout

A ring buffer is an array of `capacity` slots and two cursors: `head`, the next slot to read, and
`tail`, the next slot to write. Both move forward only, and wrap to slot 0 after the last slot.

<figure class="diagram">
{{#include figures/ring-buffer-cursors.svg}}
<figcaption>A ring of capacity 8 holding four elements, then the same ring after four more pushes. The tail wraps past slot 7 and catches up with the head: full, yet <code>head = tail</code>, exactly as when it is empty.</figcaption>
</figure>

When `head = tail`, the buffer is either empty or full, and the cursors alone cannot tell which.
There are three standard ways out:

1. Keep a separate `length` field.
2. Never fill the last slot, so `head = tail` always means empty, at the cost of one slot.
3. Make `head` and `tail` counters that never wrap in practice (`u64`), and derive the slot as
   `counter mod capacity`. Then `length = tail - head`, and empty and full are `0` and
   `capacity`. The LMAX Disruptor works this way, with sequence numbers.

With option 3 and a capacity that is a power of two, `counter mod capacity` is
`counter & (capacity - 1)`, a single instruction.

### Push

1. If `length = capacity`, the buffer is full. Either return an error so the producer waits
   (back-pressure), or, in the overwriting variant, advance `head` by one, dropping the oldest
   element.
2. Write the element into slot `tail mod capacity`.
3. Advance `tail` by one.

### Pop

1. If `length = 0`, return nothing.
2. Take the element out of slot `head mod capacity`, leaving the slot empty.
3. Advance `head` by one.

### Batch read and write

A WAL buffer is drained in bulk: everything from `head` to `tail` is written to the file in one go.
The occupied region is contiguous unless it wraps, in which case it is two slices, from `head` to
the end of the array and from slot 0 to `tail`. Writing both slices, then advancing `head` past
them, moves a whole batch with at most two copies.

<figure class="diagram">
{{#include figures/ring-buffer-batch.svg}}
<figcaption>With counters <code>head = 6</code> and <code>tail = 11</code>, the five elements occupy slots 6 to 7 and 0 to 2. Draining writes the two slices one after the other, which keeps the elements in push order.</figcaption>
</figure>

### Variants

- **Bounded queue (back-pressure):** a full buffer rejects or blocks the producer. Used where losing
  data is not acceptable, such as a log buffer.
- **Overwriting ring:** a full buffer drops its oldest element. Used for recent-history buffers
  such as metrics or trace events.
- **Single-producer, single-consumer (SPSC):** with exactly one writer of `tail` and one writer of
  `head`, each cursor can be an atomic that only its owner stores to, and no lock is needed.
- **LMAX Disruptor:** the slots are preallocated entries that are reused rather than moved,
  producers claim sequence numbers, and each consumer tracks its own sequence, so several consumers
  can read the same entries.
- **Growable ring:** Rust's `VecDeque` is a ring buffer that reallocates when full, so it gives up
  the fixed memory bound.

## Invariants

- `0 ≤ tail - head ≤ capacity` at all times.
- Elements come out in the order they went in.
- A push to a full bounded buffer fails without changing it; a pop from an empty buffer fails
  without changing it.
- In the overwriting variant, the buffer holds the last `min(pushes, capacity)` elements pushed.
- The capacity, and the allocation behind it, never change after creation.

## Implementation pitfalls

- **Empty and full look the same.** If the cursors wrap at `capacity` and there is no length field,
  a full buffer reads as empty and its contents are lost. Pick one of the three fixes above
  deliberately.
- **Counters that wrap with a capacity that is not a power of two.** If `u64` counters do wrap,
  `counter mod capacity` stays continuous across the wrap only when `capacity` divides `2⁶⁴`, that
  is when it is a power of two. Either require a power of two or make sure the counters cannot wrap.
- **Capacity 0.** `mod 0` divides by zero and `capacity - 1` underflows. Take the capacity as a
  `NonZeroUsize`, or reject 0 in the constructor.
- **Arithmetic that clippy rejects.** This crate denies `%` and `-` that can panic or overflow. Use
  `wrapping_sub` for `tail - head` with wrapping counters, a mask or `checked_rem` for the slot, and
  `get` and `get_mut` instead of `slots[i]`.
- **Leaving popped values in their slots.** If a slot still holds the popped value, it is not
  dropped until overwritten, which can keep large buffers alive. Store `Option<T>` and pop with
  `Option::take`.
- **Lock-free rings need `unsafe` in Rust.** A lock-free SPSC ring shares slots between threads
  without a lock, which in practice means `UnsafeCell` and uninitialised memory. `unsafe` is
  forbidden in this crate, so put the slots behind a `Mutex`, or use an existing audited queue such
  as `crossbeam`'s `ArrayQueue`.
- **Memory ordering.** In a concurrent ring, the producer must write the slot before publishing the
  new `tail` with a `Release` store, and the consumer must read `tail` with an `Acquire` load before
  reading the slot. With `Relaxed` on both, the consumer can see the new `tail` and an old slot.
- **False sharing.** If `head` and `tail` share a cache line, the producer and consumer keep
  invalidating each other's cache even though they write different variables. Pad them apart, for
  example with `crossbeam_utils::CachePadded`.

## Testing

- Reference model: compare against a `VecDeque` that refuses pushes beyond `capacity` (or drops its
  front, for the overwriting variant). Apply random sequences of pushes and pops to both, and
  compare every result and the length.
- Wrap-around: push and pop many times the capacity, so the cursors wrap repeatedly, and check
  order is preserved. Start counters near `u64::MAX` to exercise counter wrap.
- Batch reads: fill the buffer so the occupied region wraps, and check that the two slices together
  equal the model's contents.
- Edge cases: capacity 1, pushing to a full buffer, popping from an empty one, and dropping a
  buffer that still holds elements (count drops with a type whose `Drop` increments a counter).
- Concurrency: one producer pushes `0..n`, one consumer pops, and the consumer must see exactly
  `0..n` in order. Check the memory orderings of a small instance with `loom`.

## Further reading

- [LMAX Disruptor technical paper](https://lmax-exchange.github.io/disruptor/disruptor.html),
  Thompson et al. A ring buffer built around cache lines and sequence counters instead of locks.
