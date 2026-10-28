# Load only a bounded preview of large inputs

> Temporary draft of a pull request description. Delete this file before merging.

## Problem

Large JSON Lines (ndjson) dumps made jnv run out of memory. jnv read the whole
input into a string, then parsed all of it twice (once for the viewer, once for
completions), and every jq query cloned each value again. `max_streams` did not
help: the full input was read before the cap was applied.

## Request and how it is addressed

| Request | Addressed by | Verified |
|---|---|---|
| A 5 GB ndjson file opens and previews | Inputs over `json.max_load_bytes` (default 64 MiB) load only the leading complete JSON values that fit in that many bytes. The rest is never read, from a file or from stdin. | Records render about 1.1 s after launch. jq queries run and return results. |
| Peak memory stays flat | The amount loaded no longer depends on input size. | Peak RSS is about 1.79 GB for 200 MB, 1 GB and 5 GB inputs. |
| Small files behave like today | Inputs up to the limit take the old path: read in full, same UTF-8 check, same error. | On a 1 MB file the old and new builds render the same screen, return identical query results, and use the same memory. |

## Changes

- `src/json.rs`: new `read_bounded`, plus unit tests.
  - Reads at most `limit + 1` bytes. The extra byte shows whether more input follows.
  - If the input fits, it is returned whole, exactly as before.
  - Otherwise the text is cut after the last complete JSON value in the buffer. Values are scanned with `IgnoredAny`, so the scan does not allocate them.
  - A value that ends exactly at the cut is dropped, because it may be truncated (for example, `12` read from `12345`).
  - If not even one value fits, the limit doubles until one does. A single large JSON document therefore still loads whole, as it does today.
  - A syntax error inside the loaded prefix is reported as an error.
  - `max_streams` is respected, and `max_streams = 0` stops at the first chunk (see the metrics below).
- `src/main.rs`: config is loaded before input, so the limit is known when reading starts. `parse_input` calls `read_bounded`.
- `src/config.rs`: new `json.max_load_bytes` field. It has a serde default, so existing config files without it still load.
- `src/guide.rs` and `src/json_viewer.rs`: when only a preview is loaded, the status line says so at startup and after every query: *"Input too large: previewing the first N JSON values (X MiB); queries run on this preview only"*.
- `default.toml` and `README.md`: document `max_load_bytes`.

## Observed metrics

**Environment:** Linux, 2 vCPUs, 13 GB RAM, rustc 1.99.1, `--release` build.

**Method:**
- Each run starts jnv in a pseudo-terminal (120×40) and records peak RSS (`VmHWM` from `/proc`) until a few seconds after the first render.
- Test data is synthetic ndjson at about 215 bytes per record. The 5 GB file is five copies of the 1 GB file joined together.

### Peak memory

| Input | Before | After |
|---|---|---|
| 1 MB | 32 MiB | 32 MiB |
| 200 MB | 5,323 MiB | 1,792 MiB |
| 1 GB | not run | 1,791 MiB |
| 5 GB (file) | not run | 1,792 MiB |
| 5 GB (stdin) | not run | 1,792 MiB |
| 5 GB, running `select(.score > 99.9) \| .user` | not run | 1,791 MiB |

The 1 GB and 5 GB inputs were not run on the old build: 200 MB already used 5.3 GB on this 13 GB machine. Time from launch to first render with the new build was 1.1–1.5 s for all large inputs. In preview mode, the 5 GB file loaded 309,890 values (64.0 MiB).

### Small-file equivalence (1 MB, old build vs new build)

| Check | Result |
|---|---|
| Initial screen (ANSI codes removed) | Identical |
| `.user.name` | Identical (41 names rendered) |
| `select(.score > 99) \| .user` | Identical (12 users rendered) |
| Preview notice shown | No, in either build |

### Bug found in review: `max_streams = 0`

With `max_streams = 0`, the first version of `read_bounded` never accepted a value. It kept doubling the limit until it had read the whole input.

| Build | Peak RSS on 5 GB with `max_streams = 0` |
|---|---|
| Before the fix (`a71f4ab`) | 4,774 MiB and still rising after 20 s |
| After the fix (`ca44e61`) | 69 MiB |

### Tests

`cargo test`: 8 unit tests pass. `cargo clippy --all-targets` and `cargo fmt --check` are clean.

## Trade-offs and known limitations

- **Queries see only the preview.** For inputs over the limit, a filter such as `select(...)` can miss matches further into the file. Copying results and `--write-to-stdout` also output only preview results. The status line warns about this after each query.
- **Files just over 64 MiB now open as a preview.** A 100 MB file, for example, used to load in full. Raise `json.max_load_bytes` to change where this starts.
- **The memory ceiling is about 28× `max_load_bytes`.** This is jnv's existing cost per byte (string, two parses, viewer rows), so 64 MiB gives about 1.8 GB and 16 MiB gives about 450 MB. Lowering the multiplier, for example by sharing parsed values with the completion loader, is a separate change.
- **A single JSON document bigger than the limit still loads in full**, as it does today, because there is nothing smaller to preview.
- **Possible follow-up:** run queries over the whole file by streaming it from disk and capping only the number of results kept. Memory would stay flat, but each query would scan the full file.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
