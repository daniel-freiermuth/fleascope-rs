# FleaScope RS

A Rust library for configuring triggers and communicating with FleaScope oscilloscope devices.

This library is a complete port of the Python `pyFleaScope` library to Rust, providing idiomatic Rust APIs for device control, data acquisition, and calibration management.

## Features

- **Cross-platform device discovery**: Uses `serialport` for finding FleaScope devices across Windows, Linux, and macOS
- **Trigger configuration**: Digital and analog triggers with builder patterns and type safety
- **Data acquisition**: Raw oscilloscope data reading with automatic time indexing
- **Probe calibration**: Automated zero and 3.3V calibration procedures
- **Calibration management**: Read/write probe calibrations from/to device flash memory
- **DataFrame output**: Uses `polars` for efficient data handling (replacing pandas)
- **Type safety**: Strong typing and comprehensive error handling throughout
- **Memory efficiency**: Iterator-based device discovery for reduced memory usage
- **Comprehensive testing**: Full test coverage with both unit tests and documentation tests

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
fleascope-rs = { path = "path/to/fleascope-rs" }
```

## Quick Start

### Basic Connection and Data Reading

```rust
use fleascope_rs::{FleaScope, Waveform};
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connect to any available FleaScope device
    let mut scope = FleaScope::connect(None, None, true)?;
    
    // Set up signal generator
    scope.set_waveform(Waveform::Sine, 1000)?; // 1kHz sine wave
    
    // Read data using the 1x probe with default auto trigger
    let data = scope.read_x1(Duration::from_millis(10), None, None)?;
    println!("Captured {} samples", data.height());
    
    Ok(())
}
```

### Device Discovery

```rust
use fleascope_rs::FleaConnector;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // List available devices (memory efficient iterator)
    let devices = FleaConnector::get_available_devices(None)?;
    for device in devices.take(3) { // Only process first 3 devices
        println!("Found device: {} at {}", device.name, device.port);
    }
    
    // Or get all devices as a Vec (if you need to access multiple times)
    let devices_vec = FleaConnector::get_available_devices_vec(None)?;
    println!("Total devices: {}", devices_vec.len());
    
    Ok(())
}
```

### Digital Triggers

```rust
use fleascope_rs::{FleaScope, DigitalTrigger, BitState};
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut scope = FleaScope::connect(None, None, true)?;
    
    // Create a digital trigger that starts capturing when bit0 is high and bit1 is low
    let trigger = DigitalTrigger::start_capturing_when()
        .bit0(BitState::High)
        .bit1(BitState::Low)
        .bit2(BitState::DontCare)  // Ignore bit2
        .starts_matching();        // Start capturing when pattern matches
    
    // Read data with digital trigger
    let data = scope.read_x1_digital(
        Duration::from_millis(5), 
        Some(trigger),
        None
    )?;
    
    println!("Captured {} samples with digital trigger", data.height());
    Ok(())
}
```

### Analog Triggers

```rust
use fleascope_rs::{FleaScope, AnalogTrigger};
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut scope = FleaScope::connect(None, None, true)?;
    
    // Create an analog trigger that starts on rising edge at 1.5V
    let trigger = AnalogTrigger::start_capturing_when()
        .rising_edge(1.5);
    
    // Read data with analog trigger using 10x probe
    let data = scope.read_x10_analog(
        Duration::from_millis(5),
        Some(trigger),
        Some(Duration::from_micros(100)) // 100μs delay
    )?;
    
    println!("Captured {} samples with analog trigger", data.height());
    Ok(())
}
```

### Calibration

```rust
use fleascope_rs::FleaScope;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut scope = FleaScope::connect(None, None, true)?;
    
    // Calibrate the 1x probe
    println!("Connect 1x probe to ground and press Enter...");
    std::io::stdin().read_line(&mut String::new())?;
    let zero_value = scope.calibrate_x1_zero()?;
    println!("Zero calibration: {}", zero_value);
    
    println!("Connect 1x probe to 3.3V and press Enter...");
    std::io::stdin().read_line(&mut String::new())?;
    let full_scale = scope.calibrate_x1_3v3()?;
    println!("Full scale calibration: {}", full_scale);
    
    // Save calibration to device flash
    scope.write_x1_calibration_to_flash()?;
    println!("Calibration saved to flash");
    
    Ok(())
}
```

## API Reference

### Core Types

- **`FleaScope`**: Main oscilloscope control interface
- **`FleaConnector`**: Device discovery and connection management
- **`FleaTerminal`**: Low-level serial communication
- **`FleaProbe`**: Probe calibration and voltage conversion
- **`DigitalTrigger`**: Digital pattern triggers
- **`AnalogTrigger`**: Analog level/edge triggers

### Data Acquisition Methods

- `read_x1()`: Read with 1x probe and auto trigger
- `read_x10()`: Read with 10x probe and auto trigger
- `read_x1_digital()`: Read with 1x probe and digital trigger
- `read_x10_digital()`: Read with 10x probe and digital trigger
- `read_x1_analog()`: Read with 1x probe and analog trigger
- `read_x10_analog()`: Read with 10x probe and analog trigger

### Trigger Configuration

Digital triggers support 9 bits (bit0-bit8) with states:
- `BitState::High`: Bit must be high
- `BitState::Low`: Bit must be low
- `BitState::DontCare`: Ignore this bit

Trigger behaviors:
- `starts_matching()`: Start when pattern first matches
- `stops_matching()`: Start when pattern stops matching
- `is_matching()`: Continuously capture while matching

Analog triggers support:
- `rising_edge(voltage)`: Trigger on rising edge
- `falling_edge(voltage)`: Trigger on falling edge
- `level(voltage)`: Trigger on level crossing

## Error Handling

The library uses `thiserror` for comprehensive error handling:

```rust
use fleascope_rs::{FleaScope, FleaScopeError};

match FleaScope::connect(None, None, true) {
    Ok(scope) => println!("Connected successfully"),
    Err(FleaScopeError::Connector(e)) => println!("Connection failed: {}", e),
    Err(FleaScopeError::SerialTerminal(e)) => println!("Serial error: {}", e),
    Err(e) => println!("Other error: {}", e),
}
```

## Data Output

All data acquisition methods return a `polars::DataFrame` with columns:
- `time`: Time in seconds from trigger
- `bnc`: Voltage values (converted from raw ADC values)
- `bitmap`: Raw digital bit values (hex string)

For digital data analysis, use `FleaScope::extract_bits()` to convert the bitmap column into individual bit columns (`bit_0`, `bit_1`, etc.).

## Platform Support

This library supports Windows, Linux, and macOS through the `serialport` crate. Device discovery automatically handles platform-specific USB device enumeration.

## Dependencies

- `serialport`: Cross-platform serial port communication
- `polars`: High-performance DataFrame library
- `csv`: CSV parsing for data acquisition
- `chrono`: Time and duration handling
- `thiserror`: Error handling
- `log`: Logging support

## Differences from Python Version

- **Memory efficiency**: Iterator-based device discovery
- **Type safety**: Strong typing prevents many runtime errors
- **Performance**: Rust's zero-cost abstractions and polars for data handling
- **Error handling**: Comprehensive error types with context
- **Cross-platform**: Better cross-platform device discovery
- **API consistency**: More consistent method naming and parameter ordering

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Add tests for new functionality
5. Run `cargo test` and `cargo clippy`
6. Submit a pull request

## License

This project maintains the same license as the original Python version.

## Testing

Run the full test suite:

```bash
cargo test
```

Run tests with output:

```bash
cargo test -- --nocapture
```

Run clippy for additional code quality checks:

```bash
cargo clippy
```

Format code:

```bash
cargo fmt
```

## Examples

See the `examples/` directory for more comprehensive usage examples and real-world scenarios.
