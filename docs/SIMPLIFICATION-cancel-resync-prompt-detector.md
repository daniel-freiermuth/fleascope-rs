# Simplification plan: one prompt detector for cancel()/resync()

Status: RFC, needs a human decision (see "The question").
Base commit: `002b69d` (`src/serial_terminal.rs`).

## What exists

`src/serial_terminal.rs` has two loops that read the serial port until the
StickOS prompt `"> "` (`PROMPT`) arrives:

| Loop | Buffer | Prompt check | Read-error handling |
|---|---|---|---|
| `StatelessFleaTerminal::read_chunk` (line 78) | caller's `Vec<u8>`, grows | suffix of the `Vec` equals `PROMPT` | `TimedOut`/`Ok(0)` → no data; `BrokenPipe`/`UnexpectedEof` → `ConnectionLostError`; other → `tracing::info!` then `panic!("Serial read error: {e}")` |
| `StatelessFleaTerminal::await_prompt_and_flush` (line 190) | `VecDeque<u8>` sliding window of `PROMPT.len()` bytes, maintained by `prompt_window_extend` (line 382), plus its own `PROMPT_LEN`/`BUFFER_LEN` consts and 1024-byte read buffer | window equals `PROMPT` | its own copy: `TimedOut`/`Ok(0)` → no data; `BrokenPipe`/`UnexpectedEof` → `FleaTerminalError::ConnectionLost`; other → `panic!("Serial read error: {e}")` (no log line) |

### Why the second loop was built

`cancel()` must not accumulate the rest of a capture stream it is aborting,
so it got its own bounded read loop. Since then both loops have been fixed
separately for the same problems:

- `c3f09b8` copied `read_chunk`'s `BrokenPipe`/`UnexpectedEof` arms into the
  cancel loop.
- `6016492` (test) / `254b24f` (fix) repaired the hand-rolled sliding
  window, a bug class that cannot occur in `read_chunk`'s suffix check.
- `3f867d8` and `1780ea9` added and then repaired the loop's timeout; that
  logic belongs to the loop, not to the detector, and survives this plan.

### Payers today (re-verified repo-wide at `002b69d`)

`grep -rn 'read_chunk\|await_prompt_and_flush\|prompt_window_extend\|VecDeque'`
over the whole worktree (sources, `Cargo.toml`, `README.md`; there are no
scripts, configs or other docs that name them) finds only
`src/serial_terminal.rs`:

- `read_chunk`: `StatelessFleaTerminal::exec_sync` (line 138) and
  `BusyFleaTerminal::try_get_result` (line 366).
- `await_prompt_and_flush`: `StatelessFleaTerminal::resync` (line 186, reached
  from `IdleFleaTerminal::ensure_synced`) and `BusyFleaTerminal::cancel`
  (line 325).
- `prompt_window_extend`: line 199 in production, plus the four unit tests
  `prompt_window_single_byte_reads`, `prompt_window_slides_through_junk`,
  `prompt_window_large_read`, `prompt_window_straddles_reads` (lines 392-419).
- `VecDeque`: only the import (line 2), line 194, the helper and its tests.

All are private (`fn`, not `pub fn`); nothing outside the module or crate can
reach them.

## The simpler design

`read_chunk` stays exactly as it is. `await_prompt_and_flush` calls it on a
scratch `Vec<u8>` and, after every non-prompt chunk, keeps only the last
`PROMPT.len() - 1` bytes, so a prompt split across two reads is still found
and at most 1024 + 1 bytes are held:

```rust
fn await_prompt_and_flush(&mut self, timeout: Duration) -> Result<(), FleaTerminalError> {
    let cancel_start = Instant::now();
    let mut tail = Vec::new();
    loop {
        match self.read_chunk(&mut tail) {
            Ok(true) => break,
            // Keep only bytes that could begin a prompt split across reads.
            Ok(false) => {
                tail.drain(..tail.len().saturating_sub(PROMPT.len() - 1));
            }
            Err(ConnectionLostError) => return Err(FleaTerminalError::ConnectionLost),
        }
        if cancel_start.elapsed() >= timeout {
            return Err(FleaTerminalError::Timeout { timeout });
        }
    }
    self.flush()
}
```

`read_chunk` grows no flag or option; the adaptation lives in the caller.

### What would be deleted (production code only)

- `prompt_window_extend` (6 lines + blank).
- In `await_prompt_and_flush`: `PROMPT_LEN`, `BUFFER_LEN`, the `VecDeque`
  window and its read buffer, the duplicated `io::ErrorKind` match and the
  window comparison.
- `use std::collections::VecDeque;`.

A throwaway prototype of this design, measured against `002b69d` with
`git diff -U0` (not committed): production code +7 / -34 lines (net -27);
tests -29 lines (the four helper tests) and +1 line (`use
std::collections::VecDeque;` moves into `mod tests`, which still uses it for
the `FakeDevice` reply queue).
Concepts removed: the second prompt detector, the `VecDeque` sliding window,
the second copy of the read-error classification.

Tests that would go: the four `prompt_window_*` tests. They test the private
helper's windowing, not a behaviour of `cancel()`/`resync()`, and leave with
the helper. Behavioural tests that would stay unchanged:
`cancel_times_out_when_device_stays_silent`,
`cancel_on_unplugged_port_returns_error`,
`cancel_unplugged_after_prompt_returns_error` and the `exec_*` resync tests
(`exec_sync_timeout_does_not_leak_late_reply_into_next_command`,
`exec_async_failed_write_does_not_corrupt_next_command`,
`exec_async_failed_resync_does_not_corrupt_next_command`,
`exec_sync_failed_resync_does_not_leak_late_reply_into_next_command`,
`exec_sync_timeout_stays_bounded_by_callers_timeout`,
`exec_sync_recovers_after_timeout_with_short_caller_timeout`).

With the prototype applied, `cargo +1.95.0 test --lib serial_terminal`
passed all 12 remaining tests (the default stable 1.100 toolchain fails to
build the `ethnum` dependency of `polars`, unrelated to this change).

## What changes, and for whom

Identical for the callers: prompt detection (including a prompt straddling two
reads, via the retained tail), `TimedOut`/`Ok(0)` → keep waiting,
`BrokenPipe`/`UnexpectedEof` → `FleaTerminalError::ConnectionLost`, the
timeout check, the final `flush()`, and the panic message text.

Observable differences, all seen by users of `BusyFleaTerminal::cancel()` and
of `IdleFleaTerminal::exec_sync()`/`exec_async()` when a resync is pending:

1. **Extra log line before a panic.** An unexpected `io::Error` (any kind other
   than `TimedOut`, `BrokenPipe`, `UnexpectedEof`) during cancel/resync would
   emit `tracing::info!("Serial read error (kind: {:?})...{e}")` before
   panicking. Today it panics without it. Seen by anyone with a `tracing`
   subscriber installed. The path already aborts.
2. **Extra profiling scopes.** `read_chunk` opens `profiling::scope!("read_chunk")`
   and `"process_chunk_data"`. This crate enables no `profiling` backend
   (`Cargo.toml`: `profiling = "1.0"`, no features), so the scopes are no-ops
   here; a downstream binary that enables a backend (e.g. puffin/tracy) would
   see these scopes during cancel/resync where it sees none today.

## The question

Is it acceptable that `cancel()` and the idle-terminal resync gain
`read_chunk`'s `tracing::info!` line on the unexpected-read-error panic path
(and its profiling scopes, visible only to downstream builds with a profiling
backend)? If yes, apply the design above as-is. If the log line must not
appear there, the two loops should stay separate: making `read_chunk`
conditional on its caller would add a flag that only cancel/resync use, which
is more concepts, not fewer.
