# Simplification plan: one guard per time-frame bound

Status: RFC. A human has to answer the question at the end before this is applied.

## What exists

`IdleFleaScope::prepare_read_command` and `IdleFleaScope::number1_to_prescaler`
(`src/flea_scope.rs`) check the capture time frame five times:

| # | Where | Check | Error |
|---|-------|-------|-------|
| 1 | `prepare_read_command`, before the delay check | `time_frame.as_secs_f64() > 3.49` | `TimeFrameTooLarge` |
| 2 | `prepare_read_command`, before the delay check | `as_secs() == 0 && as_micros() < 111` | `TimeFrameTooSmall` |
| 3 | `prepare_read_command`, **after** the delay check | `number1 == 0` | `TimeFrameTooSmall` |
| 4 | `number1_to_prescaler` | `t == 0` | `TimeFrameTooSmall` |
| 5 | `number1_to_prescaler` | `t > 65535` | `TimeFrameTooLarge` |

Because of 4 and 5, `number1_to_prescaler` returns `Result<u32, CaptureConfigError>` and its
caller propagates it with `?`.

## Why it was built

- Guards 1, 2, 4 and 5 were ported together from the Python library in 3df85e1
  "First draft of porting actual FleaScope lib".
- Guard 3 came from 8e8c1d0 "ref: Use proper uint types for prescaler calculations", which
  replaced the float `number1 <= 0` check with unsigned arithmetic and mapped it to
  `TimeFrameTooSmall`. Since then the guards overlap.

## Payers today

Searched the whole repository (`src/`, `README.md`, `Cargo.toml`, CI workflows, `clippy.toml`):

- `number1_to_prescaler`: one production caller, `prepare_read_command`, and one test,
  `tests::test_number1_to_prescaler`, which asserts `number1_to_prescaler(100).is_ok()` and
  `number1_to_prescaler(0).is_err()`. Production never passes 0, because guard 3 rejects it first.
- `prepare_read_command`: called only by `read_async` and `read_sync`, both public API.
- `TimeFrameTooSmall` / `TimeFrameTooLarge`: also raised only here. Their `Display` text
  ("min 111 microseconds", "max 3.49 seconds") is not changed by this plan.

## Evidence (re-verified)

The constants are `MSPS = 18`, `MCU_MHZ = 120.0`, `INTERLEAVE = 5`, `TOTAL_SAMPLES = 2000`.

- `number1 = 18 * micros / 2000` is 0 exactly for `micros <= 111`. Every input that guard 2
  rejects is also rejected by guard 3, with the same variant.
- Guard 1 caps `micros` at 3 490 000, so `number1 <= 31410`. For every `number1` in
  `1..=31410`, `t` lies in `33..=65438`. Guards 4 and 5 cannot fire from the only caller.
- A throwaway test (since deleted) compared the current `prepare_read_command` with the simplified
  version below, comparing the full `Ok` value (effective MSPS and command string) or the
  error, for every whole microsecond from 0 to 3.6 s, at +0 ns and +999 ns, against delays
  `None, 0, 500 µs, 500 ms, 1 s, 1 s + 1 ns, 2 s`, which is 50 400 014 inputs. It also checked
  the prescaler for every `number1` in `1..=31410`. Output:

  ```
  checked 50400014 inputs; t range 33..=65438; 2 differing inputs
  us=111 delay=Some(1.000000001s): old [err Delay too large (max 1 second)] new [err Time frame too small (min 111 microseconds)] x2
  us=111 delay=Some(2s): old [err Delay too large (max 1 second)] new [err Time frame too small (min 111 microseconds)] x2
  ```

## Why this is a plan and not a change

Guards 4 and 5 can be deleted without any change in behavior. Guard 2 cannot. When the
time frame is invalid **and** the delay is over 1 s, the error you get depends on which check
runs first:

- time frame in `[0, 111 µs)`: guard 2 runs before the delay check → `TimeFrameTooSmall`
- time frame in `[111 µs, 112 µs)`: guard 2 lets it through, the delay check runs →
  `DelayTooLarge`; only after that would guard 3 have rejected it

A single `number1 == 0` guard cannot reproduce that 1 µs window. Wherever it goes, one of the two
ranges changes the error variant `read_sync` / `read_async` return:

- **Shape A (recommended)**: move `number1 == 0` up to where guard 2 is now, before the delay
  check. Time frames in `[111 µs, 112 µs)` with a delay over 1 s change from
  `DelayTooLarge` to `TimeFrameTooSmall`. Time-frame errors then always win over delay
  errors, which is what the "Validate time frame" / "Validate delay" order already means.
  This is the run measured above.
- **Shape B**: leave `number1 == 0` after the delay check. Every time frame in
  `[0, 111 µs)` with a delay over 1 s changes from `TimeFrameTooSmall` to `DelayTooLarge`,
  which is a larger change.
- **Shape C**: delete only guards 4 and 5 and the `Result` around `number1_to_prescaler`, and
  keep guard 2. No behavior changes, but two `TimeFrameTooSmall` rules remain.

Every input that is valid, or invalid for only one reason, gives the same result in all three
shapes.

## Simpler design (shape A)

```rust
/// Convert number1 to prescaler value.
///
/// Needs no range check of its own: `prepare_read_command` only passes
/// `number1` in `1..=31410` (time frame over 111 µs, at most 3.49 s), which
/// keeps `t` in `33..=65438`, inside the hardware's 16-bit range.
fn number1_to_prescaler(number1: u32) -> u32 {
    let ps = if number1 > 1000 { 16 } else { 1 };
    let t = ((Self::MCU_MHZ * f64::from(number1 * Self::INTERLEAVE)
        / f64::from(ps)
        / f64::from(Self::MSPS))
        + 0.5) as u32;

    ps * t
}

// in prepare_read_command:
// Validate time frame. With MSPS = 18 and TOTAL_SAMPLES = 2000, number1
// is 0 exactly for time frames up to 111 µs, and the 3.49 s cap keeps it
// at most 31410; re-check both if these constants change.
if time_frame.as_secs_f64() > 3.49 {
    return Err(CaptureConfigError::TimeFrameTooLarge);
}
let number1 = Self::MSPS * (time_frame.as_micros() as u32) / Self::TOTAL_SAMPLES;
if number1 == 0 {
    return Err(CaptureConfigError::TimeFrameTooSmall);
}

// Validate delay
if delay.as_secs_f64() > 1.0 {
    return Err(CaptureConfigError::DelayTooLarge);
}

let prescaler = Self::number1_to_prescaler(number1);
```

`test_number1_to_prescaler` keeps both of its assertions, now made through the caller:
`prepare_read_command` with 11 112 µs (`number1 = 100`) is `Ok`, and with 111 µs it returns
`TimeFrameTooSmall`.

## What would be deleted (production code only, comments and blanks excluded)

Measured on the shape A patch: 14 code lines removed and 5 added, so about 9 fewer.
- guard 2 (the `as_micros() < 111` condition; its `return` / `}` are reused by the moved
  `number1 == 0` check)
- guards 4 and 5 (6 lines)
- the `Result` return type of `number1_to_prescaler`, its `Ok(..)` wrapping and the `?` at the
  call site

That is three branches and one fallible signature fewer. Shape C would remove guards 4 and 5
and the `Result` (6 code lines fewer) and keep guard 2.

Risk in every shape: the remaining guards only cover the deleted ones while the constants stay
as they are. If `MSPS` or `TOTAL_SAMPLES` change, `t` could leave the 16-bit range. That is why
the comment next to the check names the constants.

## Question for the human

When a capture request has **both** a time frame in `[111 µs, 112 µs)` **and** a delay over
1 s, may `read_sync` / `read_async` return `TimeFrameTooSmall` instead of today's
`DelayTooLarge` (shape A)? Or should no error variant change at all, keeping the
`micros < 111` guard and deleting only the two prescaler checks (shape C)?
