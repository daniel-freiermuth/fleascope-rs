# FleaScope RS

Fast Rust library for FleaScope oscilloscope devices. High-performance data acquisition with dual probes, digital/analog triggers, and automatic calibration.

## Perks

- Fearless state handling, guarded by type states
- read_async for cancellable reads
- Cross-Platform: Better device discovery and serial communication across Windows/Linux/macOS.
- Output as Polars frames

## Quick Start

Connect and read data in just a few lines:

```rust
use fleascope_rs::{IdleFleaScope, DigitalTrigger};
use std::time::Duration;

// Connect and acquire data
let mut (scope, x1, x10) = IdleFleaScope::connect("FleaScope", None, true)?;
let trigger_config = DigitalTrigger::start_capturing_when()
                        .is_matching()
                        .into_trigger_fields()?;
let reading = scope.read_sync(Duration::from_millis(10), trigger_config, None)?;
let uncalibrated_frame : Polars::LazyFrame = reading.parse_csv();
let calibrated_frame = x1.apply_calibration(uncalibrated_frame)
println!("Captured {} samples", calibrated_frame.collect().height());
```

## Device Discovery

```rust
use fleascope_rs::FleaConnector;

let devices = FleaConnector::get_available_devices(None)?;
for device in devices.take(3) {
    println!("Found: {} at {}", device.name, device.port);
}
```

## Triggers

**Digital Triggers** - Pattern matching on 9 bits:
```rust
use fleascope_rs::{DigitalTrigger, BitState};

let trigger = DigitalTrigger::start_capturing_when()
    .bit0(BitState::High)
    .bit1(BitState::Low) 
    .bit2(BitState::DontCare)
    .starts_matching();

let data = scope.read_sync(Duration::from_millis(5), trigger.into_trigger_fields(), None)?;
```

**Analog Triggers** - Edge and level detection:
```rust
use fleascope_rs::AnalogTrigger;

let trigger = AnalogTrigger::start_capturing_when().rising_edge(1.5);
let data = scope.read_sync(Duration::from_millis(5), trigger.into_trigger_fields(), None)?;
```

## Calibration

```rust
let mut (scope, x1, x10) = FleaScope::connect(None, None, true)?;

// Connect probe to ground, then 3.3V
let zero_value = x1.calibrate_0(scope)?;
let full_scale = x1.calibrate_3v3(scope)?;
x1.write_calibration_to_flash(scope)?; // Save to device
```

## API Overview

**Data Output:**
All methods return `polars::DataFrame` with:
- `time` - Time in seconds from trigger
- `bnc` - Voltage values (auto-converted from ADC)  
- `bitmap` - Digital bit values (hex string)

Use `IdleFleaScope::extract_bits()` to convert bitmap to individual bit columns.

## Related Projects

- Live monitor GUI https://github.com/daniel-freiermuth/fleascope-monitor-rs
- Sibling project in python https://github.com/daniel-freiermuth/pyfleascope
