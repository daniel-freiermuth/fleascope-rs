# Changelog

All notable changes to fleascope-rs are documented here. Each release states
the FleaScope firmware versions it supports.

## [1.1.0] — 2026-11-23

Supports firmware `>= 3.0.0, < 4.0.0`; digital stream needs `>= 3.1.0`.

### Added
- **Digital stream:** `IdleFleaScope::digital_stream(mask, us_per_sample,
  n_samples_per_packet)` streams the selected digital inputs as packets,
  decoded by `DigitalStreamingScope::read_packet` into one bitmap per sample;
  `IdleFleaScope::compute_n_samples_per_packet` picks a packet size for a
  ~50 ms latency. Parameters outside the firmware's limits are rejected before
  anything is sent (`CaptureConfigError::InvalidChannelMask`,
  `InvalidSamplesPerPacket`), and on firmware older than 3.1.0 with
  `CaptureConfigError::DigitalStreamUnsupported`.

## [1.0.0] — 2026-11-23

Supports firmware `>= 3.0.0, < 4.0.0`.

### Breaking
- **polars removed.** A capture is parsed when the read completes into a
  `ScopeReading` with `raw_bnc()`, `bitmap()`, `len()`, `time(i)`, `times()`
  and `bit(channel)`. `parse_csv`, `extract_bits`, the `*_COLUMN_NAME`
  constants and `apply_calibration` are gone; `FleaProbe::voltages(&[u16])`
  converts ADC counts (captures or stream batches) to volts and
  `raw_to_voltage` takes and returns `f64`. Malformed or empty captures are a
  `ParseError` instead of silently becoming `false` bits.
- **Polling a read:** `ReadingFleaScope::try_get_result()` returns
  `Result<ReadStatus, ConnectionLostError>` with `ReadStatus::Pending(_)` or
  `ReadStatus::Done(IdleFleaScope, Result<ScopeReading, ParseError>)`; the idle
  scope comes back even for a malformed capture.
- **Errors instead of panics.** Terminal commands
  (`IdleFleaTerminal::exec_sync`/`exec_async`, `cancel`), connecting, serial
  reads, probe conversions and malformed device replies return errors.
  `IdleFleaScope::new` returns `FleaConnectorError`.
- **Firmware check on connect.** `IdleFleaScope::new`/`connect` read `ver` and
  refuse firmware outside `>= 3.0.0, < 4.0.0` with
  `FleaConnectorError::UnsupportedFirmware`, or
  `FirmwareVersionUnparseable`. The 2.27x releases are reported as
  unsupported.
- `BitTriggerBuilder::set_bit` is fallible.
- The error enums (`FleaConnectorError`, `CaptureConfigError`,
  `CalibrationError`, `ParseError`, `FlashUpgradeError`, `FleaTerminalError`)
  are `#[non_exhaustive]`: matches need a wildcard arm, and later minor
  releases may add variants.
- Removed: the unused `Trigger` enum, the `TriggerConfig` trait (its
  `into_trigger_fields` is an inherent method now),
  `FleaConnectorError::DeviceValidationFailed`, the `read_calibrations` flag
  of `FleaConnector::connect` and the `puffin` feature (profiling uses the
  `profiling` crate).
- Rust 2024 edition; minimum Rust 1.95.

### Added
- **Firmware upgrade:** `IdleFleaTerminal::flash_upgrade` streams an Intel HEX
  file with progress callbacks (`FlashProgress`, `FlashUpgradeError`). It works
  on any firmware version, including ones this crate no longer supports.

### Fixed
- Commands other than captures time out after 5 s instead of hanging on a
  silent device; the port-less `connect` gives up after 3 handshake attempts.
- After a failed or timed-out command, the terminal resyncs before the next
  one instead of reading the previous reply or appending to a half-written
  line.
- `cancel()`: correct prompt detection across reads, a reachable 5 s timeout,
  and broken-pipe handling.
- Serial reads interrupted by a signal are retried instead of dropping the
  connection.
- Capture bitmaps are parsed as hex even when they contain only digits.
- Calibration integers from flash are converted without overflow.
- Response bytes are trimmed of the command echo and line endings.
