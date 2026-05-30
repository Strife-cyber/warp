# Lost Update Bug: Concurrent `warp run` and `warp add` Clobber Registry

**Severity:** Data loss — downloads silently vanish from `registry.json`

## Symptom

After running `warp add <url>` to add a new download, then later (or concurrently)
running `warp run`, the newly-added download disappears from the registry. It is
never listed, never downloaded, and is simply gone.

The user expects the added download to remain in the registry and be picked up
by the next run. Instead, it is overwritten as if it was never added.

## Reproduction (most obvious case)

Terminal 1 starts a long download:

```bash
warp add "https://example.com/large-file.iso"       # adds download A
warp run                                              # runs A — takes minutes
```

While Terminal 1 is still downloading, Terminal 2 adds another download:

```bash
warp add "https://example.com/another-file.zip"       # adds download B
warp list                                              # shows both A and B ✓
```

When Terminal 1 finishes, `warp list` shows only A. B is gone.

(It also happens sequentially if `warp add` runs between the start of `warp run`
and its final save — which is always, because the engine runs for the full duration
of all downloads.)

## Why It Happens — Walkthrough

### The architecture that enables it

The `Registry` struct (`src/registry.rs:32-37`) is the single source of truth
for all download state. It is persisted as a JSON file (`registry.json`) and
every CLI command follows the same pattern:

1.  **Load** the entire registry from disk into an in-memory `HashMap`
2.  **Modify** entries in place
3.  **Save** the entire `HashMap` back to disk, overwriting the file

All mutations (add, remove, pause, resume, retry, clean) fit inside a single
function call — load, change one entry, save, exit. They are fast and
uncontroversial.

The problem is `engine::run_all()` (`src/engine.rs`). It holds the registry in
memory for the **entire duration of all downloads** — potentially minutes or
hours — while saving the full state only twice (lines 86 and 109). Any other
`warp` process that writes to `registry.json` during that window will have its
changes silently overwritten at the next save from the original process.

### Step-by-step timeline

Let `registry.json` initially contain download A (`{A: Pending}`).

```
 Time │ Process 1 (warp run)               │ Process 2 (warp add …)
──────┼────────────────────────────────────┼────────────────────────────────
  t1  │ Registry::load()                   │
      │ → in-memory map: {A: Pending}      │
  t2  │ run_all() begins                   │
      │ Downloading A…                     │
  t3  │                                    │ Registry::load()
      │                                    │ → in-memory map: {A: Downloading}
  t4  │                                    │ registry.add("https://…")
      │                                    │ → in-memory map: {A: Downloading,
      │                                    │                      B: Pending}
  t5  │                                    │ registry.save()
      │                                    │ → writes {A: Downloading, B: Pending}
  t6  │ Download of A finishes             │
  t7  │ registry.save()                    │
      │ → writes {A: Completed}            │  ← B is silently removed!
```

At **t7**, Process 1 saves the `HashMap` it loaded at **t1** — it never saw
download B. The save is a full overwrite, so B disappears.

### The specific code

#### Entry point — `src/main.rs:27`

```rust
let mut registry = Registry::load()?;
```

The registry is loaded exactly once, before dispatching to the command handler.
For `run`, this means the snapshot is taken before any download starts and is
never refreshed.

#### The engine — `src/engine.rs`

```rust
// Line 86 — intermediate save (statuses set to Downloading)
registry.save().ok();

// …long-running downloads happen here…

// Line 109 — final save (statuses set to Completed / Error)
registry.save().ok();
```

Both calls write the *entire* `self.downloads` HashMap. The final save at line 109
is the clobber point.

#### The save function — `src/registry.rs:79-83`

```rust
pub fn save(&self) -> Result<()> {
    let path = self.get_registry_path()?;
    let data = serde_json::to_string_pretty(self)?;
    std::fs::write(path, data)?;
    Ok(())
}
```

`std::fs::write` atomically replaces the file (on most platforms), so the
clobber is instant and leaves no partial/corrupt state — it just replaces the
whole truth with a stale subset of it.

## Why Other Commands Are Not Affected

`add`, `remove`, `pause`, `resume`, `retry`, and `clean` all load the registry,
make one change, save, and exit in under a millisecond. The window for a race
is tiny, and more importantly, even if two of these commands race, the last one
to save "wins" — the changes are merged only by accident of full-file
overwrite.

The `run` command is unique because it holds the in-memory registry for the
entire download duration, making the race window enormous and the clobber
nearly guaranteed if any concurrent write happens.

## Possible Fixes (in increasing order of robustness)

These are deliberately vague — you asked to solve it yourself :)

### Fix 1: Re-load and merge before the final save

At line 109 of `engine.rs`, instead of saving the stale in-memory state, load
the *current* state from disk, then overlay only the status changes the engine
made onto it. This way any downloads added by another process are preserved.

**Downside:** You still have a tiny race between the reload and the save. Also,
if an `add` from another process used a timestamp-based ID (see
`registry.rs:89-93`), and your reload picks it up, you'd need to make sure you
don't re-process it.

### Fix 2: File-level locking

Use `fs2::FileExt::lock_exclusive()` (or platform equivalent) on
`registry.json`. Before any load, acquire the lock. Before any save, hold the
lock. This serializes all `warp` processes at the OS level — no two can touch
the registry at the same time.

**Downside:** `warp list` and `warp add` would block if a `warp run` is in
progress. That may or may not be acceptable.

### Fix 3: Save deltas instead of full state

Instead of writing the entire `HashMap`, have the engine write only the status
updates for the downloads it processed. Teach `load()` to apply these as
patches over the on-disk state.

**Downside:** More complex, introduces the concept of a journal/delta log.

### Fix 4: Transactional merge save

Write a `save_with_merge()` that:
1. Reads the current file from disk
2. Takes the in-memory `self.downloads`
3. For every key that exists **only** on disk (added by another process), keeps
   it
4. For every key that exists in both, uses the in-memory version (which has the
   updated status)
5. Writes the merged result

This makes the save non-destructive to concurrent additions.

## What to Look Out For

- **Timestamp-based IDs** (`registry.rs:89-93`). Two processes creating entries
  in the same second will have the same ID. The `HashMap` will silently drop
  one.
- **Merging statuses.** If Process 1 sets download A to `Completed`, but
  Process 2 changed A's URL or priority after that, should the save preserve
  the new URL or the completed status? This is a design question about what
  "latest write wins" means for different fields.
- **In-flight downloads known to the engine but not yet on disk.** If you
  re-load in the engine's final save, make sure you don't overwrite the
  `Downloading` → `Completed` transition with stale data from disk.
