//! # `FleaScope` RS
//!
//! A Rust library for configuring triggers and communicating with `FleaScope` oscilloscope devices.
//!
//! This library provides types and builders for creating digital and analog triggers
//! that can be used to control data capture timing in `FleaScope` oscilloscope devices,
//! as well as a serial terminal interface for communication and complete device control.
//!
//! ## Features
//!
//! - **Cross-platform device discovery**: Uses `serialport` for finding `FleaScope` devices
//! - **Trigger configuration**: Digital and analog triggers with builder patterns
//! - **Data acquisition**: Raw oscilloscope data reading with automatic time indexing
//! - **Calibration management**: Read/write probe calibrations from/to device flash
//! - **`DataFrame` output**: Uses `polars` for efficient data handling instead of pandas
//! - **Type safety**: Strong typing and error handling throughout
//!
//! ## Examples
//!
//! ### Device Connection and Basic Usage
//!
//! ```rust,no_run
//! use fleascope_rs::{IdleFleaScope, DigitalTrigger, Waveform};
//! use fleascope_rs::trigger_config::TriggerConfig;
//! use std::time::Duration;
//!
//! // Connect to any available FleaScope device
//! let (mut scope, x1_probe, x10_probe) = IdleFleaScope::connect(None, None, true)?;
//!
//! // Set up signal generator
//! scope.set_waveform(Waveform::Sine, 1000); // 1kHz sine wave
//!
//! // Read data using default auto trigger
//! let trigger = DigitalTrigger::start_capturing_when().is_matching().into_trigger_fields();
//! let reading = scope.read_sync(Duration::from_millis(10), trigger, None)?;
//! let data = reading.parse_csv()?;
//! println!("Captured {} samples", data.collect()?.height());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ### Digital Trigger
//!
//! ```rust
//! use fleascope_rs::trigger_config::{DigitalTrigger, BitState, TriggerConfig};
//!
//! let trigger = DigitalTrigger::start_capturing_when()
//!     .bit0(BitState::High)
//!     .bit1(BitState::Low)
//!     .starts_matching();
//!
//! let trigger_fields = trigger.into_trigger_fields();
//! println!("Digital trigger: {}", trigger_fields.into_string());
//! ```
//!
//! ### Analog Trigger
//!
//! ```rust
//! use fleascope_rs::{trigger_config::{AnalogTrigger, AnalogTriggerBehavior, TriggerConfig}};
//!
//! // Create analog trigger directly with raw ADC value
//! let trigger = AnalogTrigger::new(500, AnalogTriggerBehavior::Rising);
//! let trigger_fields = trigger.into_trigger_fields();
//! println!("Analog trigger: {}", trigger_fields.into_string());
//! ```
//!
//! ### Data Acquisition with Triggers
//!
//! ```rust,no_run
//! use fleascope_rs::{IdleFleaScope, DigitalTrigger, AnalogTrigger, BitState};
//! use fleascope_rs::trigger_config::{AnalogTriggerBehavior, TriggerConfig};
//! use std::time::Duration;
//!
//! let (mut scope, x1_probe, x10_probe) = IdleFleaScope::connect(None, None, true)?;
//!
//! // Read with digital trigger
//! let digital_trigger = DigitalTrigger::start_capturing_when()
//!     .bit0(BitState::High)
//!     .starts_matching()
//!     .into_trigger_fields();
//! let reading = scope.read_sync(Duration::from_millis(5), digital_trigger, None)?;
//! let data = reading.parse_csv()?;
//!
//! // Read with analog trigger (using raw ADC value)
//! let analog_trigger = AnalogTrigger::new(500, AnalogTriggerBehavior::Rising)
//!     .into_trigger_fields();
//! let reading = scope.read_sync(
//!     Duration::from_millis(10),
//!     analog_trigger,
//!     Some(Duration::from_micros(500))
//! )?;
//! let data = reading.parse_csv()?;
//!
//! // You can also read without specific bit patterns (auto trigger)
//! let auto_trigger = DigitalTrigger::start_capturing_when()
//!     .is_matching()
//!     .into_trigger_fields();
//! let reading = scope.read_sync(Duration::from_millis(5), auto_trigger, None)?;
//! let data = reading.parse_csv()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ### Device Discovery
//!
//! ```rust,no_run
//! use fleascope_rs::FleaConnector;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Connect to any available FleaScope device
//! let terminal = FleaConnector::connect(None, None, true)?;
//!
//! // Or connect to a specific port
//! let terminal = FleaConnector::connect(None, Some("/dev/ttyUSB0"), true)?;
//!
//! // List available devices (iterator - memory efficient)
//! let devices = FleaConnector::get_available_devices(None)?;
//! for device in devices.take(3) { // Only process first 3 devices
//!     println!("Found device: {} at {}", device.name, device.port);
//! }
//!
//! // Or get all devices as a Vec (if you need to access multiple times)
//! let devices_vec = FleaConnector::get_available_devices_vec(None)?;
//! println!("Total devices: {}", devices_vec.len());
//! # Ok(())
//! # }
//! ```
//! ```

pub mod flea_connector;
pub mod flea_scope;
pub mod serial_terminal;
pub mod trigger_config;

// Re-export the main types for convenience
pub use trigger_config::{
    AnalogTrigger, AnalogTriggerBehavior, AnalogTriggerBuilder, BitState, BitTriggerBuilder,
    DigitalTrigger, DigitalTriggerBehavior, Trigger,
};

pub use serial_terminal::{FleaTerminalError, IdleFleaTerminal, StatelessFleaTerminal};

pub use flea_connector::{FleaConnector, FleaConnectorError, FleaDevice};

pub use flea_scope::{FleaProbe, IdleFleaScope, ProbeType, Waveform};
