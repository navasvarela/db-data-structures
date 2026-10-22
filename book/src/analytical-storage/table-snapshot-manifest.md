# Table snapshot manifest

> Builds on [zone maps](../analytical-storage/zone-maps.md).

A tree of metadata files listing which immutable data files make up each version of a table, with
column statistics per file. A commit writes new metadata and atomically swaps the pointer to the
current version, which gives snapshot isolation and time travel on top of object storage that cannot
update files in place.

- **Used by:** Apache Iceberg (snapshot, manifest list, manifests), Delta Lake's transaction log,
  Apache Hudi's timeline.
- **Notes:** the per-file statistics act as zone maps, so a planner prunes whole files before
  opening any. An [LSM tree](../lsm-trees/lsm-tree.md)'s manifest plays the same role for its
  SSTables.

## How it works

### Layout

A table is a set of immutable data files (usually [columnar segments](columnar-segments.md)) plus
metadata saying which files belong to which version. Iceberg (format versions 1 to 3) arranges the
metadata as a tree:

<figure class="diagram">
{{#include figures/table-snapshot-manifest-tree.svg}}
<figcaption>Iceberg's metadata tree for table <code>events</code>. The catalog's pointer (blue) is the only mutable piece; everything below it is an immutable file. Data manifests list data files with their statistics, and delete manifests list deletion vectors and delete files.</figcaption>
</figure>

- **Table metadata file:** the root. It lists the schema, partitioning and all retained snapshots,
  and names the current one. The catalog stores a single pointer to the current metadata file.
- **Snapshot:** one version of the table: an ID, its parent's ID, a sequence number, a timestamp,
  and the location of its manifest list.
- **Manifest list:** the manifests that make up the snapshot, each with summaries (file counts and
  per-partition-field bounds) used to skip whole manifests.
- **Manifest:** a list of files with, for each, a status (`ADDED`, `EXISTING` or `DELETED`), its
  partition values and its column statistics, which are [zone maps](zone-maps.md) at file level.
  Manifests are reused unchanged across snapshots when their files did not change.

Delta Lake keeps the same information as a log instead of a tree. The directory `_delta_log`
holds one JSON file per version, named by the version number padded to 20 digits
(`00000000000000000042.json`). Each file lists actions such as `add` and `remove` for data files
(with statistics), `metaData` and `protocol`. Periodic checkpoint files in Parquet store the full
table state at a version, so a reader does not replay the log from version 0.

### Commit

Writers never coordinate while writing data; they coordinate only at the final pointer swap. In
Iceberg with a catalog that supports compare-and-swap:

1. Read the current metadata file, version `V`, and its current snapshot `S`.
2. Write the new data files under unique names.
3. Write a new manifest listing the new files with status `ADDED`. Rewrite an existing manifest only
   if one of its files is being removed, marking that file `DELETED`.
4. Write a new manifest list: the reused manifests plus the new ones, with their summaries.
5. Write a new metadata file `V+1` containing a new snapshot whose parent is `S`.
6. Ask the catalog to swap the table's pointer from `V` to `V+1`, but only if it still points at
   `V`. If the swap succeeds, the commit is done.
7. If it fails, another writer committed first. Reload the current metadata, check that this
   change still applies on top of it (next section), and redo steps 4 to 6. The data files and new
   manifests from steps 2 and 3 are reused.

<figure class="diagram">
{{#include figures/table-snapshot-manifest-commit.svg}}
<figcaption>An append that commits snapshot 43. The new manifest list reuses <code>m1</code> and <code>m7</code> unchanged and adds <code>m9</code> for the new files. Nothing is visible to readers until the catalog's compare-and-swap moves the pointer from v42 to v43.</figcaption>
</figure>

In Delta Lake, step 6 is creating the next log file, `N+1.json`, with an operation that fails if
the file already exists. The protocol requires that writers never overwrite an existing log entry.

### Conflict checks on retry

Retrying blindly can corrupt the table, so each operation states what must still hold. Iceberg's
specification gives the rules:

- An append has no requirements and can always be reapplied.
- A replace (such as compaction) must check that every file it removes is still in the table.
- A delete of specific files must check that those files are still there; a delete by expression
  (`ts < X`) can always be reapplied.
- A schema or partition change must check that the schema has not changed in between.

If the check fails, the operation aborts instead of retrying.

### Plan a scan

1. Load the current metadata file, or, for time travel, choose an older snapshot by ID or by
   timestamp.
2. Read the snapshot's manifest list. Skip manifests whose partition summaries rule out the
   predicate.
3. Read the remaining manifests. Keep the live entries (`ADDED` and `EXISTING`) whose partition
   values and column bounds may match the predicate.
4. Attach to each data file the deletion vectors or delete files that apply to it.
5. Hand the list of files to the readers. No data file has been opened yet.

In Delta Lake, steps 1 to 3 are: read the latest checkpoint (found through the `_last_checkpoint`
file), replay the JSON commits after it in version order, keep the newest action for each file,
and then prune the resulting `add` actions with their statistics.

### Expire snapshots and collect garbage

Old snapshots keep old files alive, so storage grows until they are expired:

1. Choose which snapshots to keep from a retention policy (Iceberg has a minimum number to keep
   and a maximum age).
2. Commit new metadata without the expired snapshots.
3. Delete data files, manifests and manifest lists that no retained snapshot references.

Delta Lake's equivalent keeps `remove` actions as tombstones until a retention period passes (7
days by default, according to the protocol), after which `VACUUM` may delete the files.

## Invariants

- A committed snapshot never changes; every file it references stays readable until the snapshot
  is expired.
- Each data file appears in at most one live entry of a snapshot (Iceberg calls this content file
  uniqueness).
- Sequence numbers increase strictly along a branch's history, and every snapshot except the first
  has a parent in the table's history.
- Delta Lake versions are contiguous integers; version `v` is version `v - 1` plus exactly the
  actions in `v.json`.
- Every file's statistics are valid bounds for the rows in that file.
- Planning a scan of snapshot `S` returns a superset of the files containing rows that match the
  predicate in `S`.

## Implementation pitfalls

- **Concurrent commits and optimistic retries.** The retry must rebuild everything that depends on
  the base version: the manifest list, the snapshot's parent and its sequence number. Iceberg makes
  this cheap by leaving sequence numbers in new manifest entries empty, to be inherited from the
  manifest list, so manifests can be reused across retries.
- **A non-atomic swap.** A rename on object storage is not an atomic compare-and-swap. Iceberg's
  specification deprecates its rename-based file system commit as unsafe on object stores and local
  file systems. Use a catalog with a conditional update, or a put-if-absent write.
- **Unknown commit outcome.** If the swap request times out, the commit may or may not have
  happened. Deleting the new files as "failed" can delete files a committed snapshot references.
  Check the current metadata before cleaning up, and when in doubt leave orphan files for a later
  cleanup job.
- **Garbage collection racing readers.** A long query, or a time-travel read, may still be reading a
  snapshot you are about to expire. Retention periods exist for this; never delete a file the
  instant it stops being current.
- **Statistics edge cases.** File-level bounds have all the problems of [zone maps](zone-maps.md):
  NaNs (Iceberg forbids them as bounds and keeps a separate NaN count), `-0.0` before `+0.0`,
  truncated strings, and stale bounds after deletes.
- **Action order inside a Delta commit.** Actions within one commit are not guaranteed to be
  applied in order. The protocol therefore forbids a commit from containing two actions of the same
  type for the same file, and reconciliation keeps the newest action per file across commits.
- **Timestamps for time travel.** Commit timestamps come from writers' clocks, which can be skewed,
  so a snapshot can carry an earlier timestamp than its parent and "the table as of time T" can be
  ambiguous. Treat timestamps as hints and the snapshot ids and parent links as the real history.
- **Small commits pile up metadata.** Every fast append adds a manifest. Periodically rewrite
  manifests (Iceberg) or write checkpoints (Delta) so planning stays fast.

## Testing

- Model-based: keep a reference map from version to the set of live files, apply random appends,
  deletes, compactions and expirations, and check that planning every retained version returns
  exactly the reference set.
- Concurrency: start two writers from the same version. Two appends must both land, one after a
  retry. A compaction racing a delete of one of its input files must abort.
- Crash injection: stop the commit after each step. The table must always read as either the old
  version or the new one, with at worst some orphan files.
- Pruning: for random predicates, every file that actually contains a matching row is in the plan.
- Delta: replaying from a checkpoint gives the same state as replaying from version 0.

## Further reading

- [Apache Iceberg table specification](https://iceberg.apache.org/spec/). Snapshots, manifest lists
  and manifests.
- [Delta Lake: High-Performance ACID Table Storage over Cloud Object
  Stores](https://www.vldb.org/pvldb/vol13/p3411-armbrust.pdf), Armbrust et al., 2020.
- [Delta Lake transaction log protocol](https://github.com/delta-io/delta/blob/master/PROTOCOL.md).
