# Preview large NDJSON streams with bounded memory

Temporary file: a draft PR description, to be removed before merging.

## Problem

Large NDJSON dumps could not be opened. Peak memory was about 21x the input
size, because the input, two parsed copies, the viewer rows and the completion
paths were all held at once. A 5 GiB file would need an estimated 108 GiB.

## What changed

Input is read up to `large_input_threshold` (256 MiB).

- **Input that ends within the threshold** takes the same path as before.
- **Larger input** is cut to its leading whole records, up to `preview_size`
  (16 MiB) or 100,000 records, and the rest is not read. A set `max_streams`
  replaces both of those limits.
- **Input with no complete record within the threshold**, such as one huge
  document, is still loaded entirely.

Both new settings have defaults, so existing `config.toml` files keep loading.
`large_input_threshold = 0` turns previews off.

## Request coverage

| Requirement | Status | Evidence |
| --- | --- | --- |
| 5 GB NDJSON file opens | Done | First render in about 0.6 s |
| Preview works | Done | Scrolling to the end, collapse, completion and filters checked on a 5 GiB file |
| Peak memory stays flat | Done, within the limits below | 350 MiB at 300 MiB, 1 GiB and 5 GiB |
| Small files behave as today | Done | Output and errors byte-identical to the old binary; same memory |

## Measurements

Release build, driven through a pty at 120x40, with peak RSS taken from
`wait4`. The data is synthetic log records of 314 bytes on average.

Peak memory at open, by file size:

| Input | Before | After | First render, after |
| --- | --- | --- | --- |
| 16 MiB | 350 MiB | 350 MiB | 0.2-0.3 s |
| 64 MiB | 1,383 MiB | 1,384 MiB | 1.0 s |
| 300 MiB | not measured | 350 MiB | 0.6 s |
| 1 GiB | not measured | 350 MiB | 0.6 s |
| 5 GiB (5,370,074,820 bytes) | about 108 GiB, estimated | 350 MiB | 0.6 s |
| 5 GiB through stdin | not measured | 350 MiB | 0.4 s |

The estimate extrapolates a measured 5.5 GiB at 257 MiB. It was not run.

Other scenarios at 5 GiB unless noted:

| Scenario | Peak |
| --- | --- |
| One simple filter (`.user`, `.`) | 350-396 MiB |
| Tab completion | 350 MiB |
| Viewer navigation | 364 MiB |
| `--write-to-stdout` | 364 MiB |
| About 90 filter runs in one session | 684 MiB (old binary on the same 16 MiB of data: 648 MiB) |
| `[.,.,.,.]`, which multiplies output | 1,083 MiB, measured before the `max_streams` fix |
| Endless pipe of `{"a":1}` | 261 MiB |
| Endless pipe of 42-byte records | 484 MiB, measured before the `max_streams` fix |
| Endless pipe of dense one-line arrays (worst shape tried) | 2,078 MiB, measured before the `max_streams` fix |
| `max_streams = 200000` | 1,305 MiB, exactly 200,000 records |

In a session, expect about 600-700 MiB rather than the 350 MiB at open. The old
binary plateaus at the same level on the same data, so this tracks the amount
of data loaded, not the file size.

## Small-file parity

Compared with the old binary, using a config file without the new keys:

- `tapes/demo.json` and a 16 MiB NDJSON file: `--write-to-stdout` output is
  byte-identical, unfiltered.
- The 16 MiB file with a filter applied: byte-identical.
- Malformed and empty input: stdout and stderr are byte-identical.
- A 340 MB single document: same output on both (it stays a full load).

## Behaviour to review

- **Results are partial on large input.** Filters, copying and
  `--write-to-stdout` cover only the preview. A notice stays in the guide, even
  with `no_hint`, and `--write-to-stdout` repeats it on stderr.
- **There is a step at the threshold.** A file just under 256 MiB loads fully,
  at over 5 GiB of memory, while one just over it uses 350 MiB. Files above the
  threshold that used to load fully on a large machine now open as previews.
- **Records past the preview cannot be reached.** Parsing errors there are not
  reported either.
- **Configuration now loads before the input**, because the input needs its
  limits. A bad config now fails before the input is read.

## Known limits

- A single huge document still loads fully. A 340 MB one peaked near 6 GiB, the
  same as before, and neither binary had exited 90 s after launch, even after
  Ctrl+C.
- Memory is bounded by the preview, not by a fixed number. Dense records use
  more, a first record of up to 256 MiB is kept whole, and one larger than that
  falls back to a full load.
- Large `max_streams` values trade memory for records, as measured above.

## Existing issues, not changed

- `--write-to-stdout` joins records with commas and leaves terminal escape codes
  in redirected output, so that output is not valid NDJSON.

## Testing

- `cargo test`: 16 tests pass (the repo had none before).
- `cargo fmt --all --check` and `cargo clippy --all-targets -- -D warnings` pass.
- Memory figures come from a local harness that is not part of this change, so
  CI does not guard them. The bounded read is covered by unit tests.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
