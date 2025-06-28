//! # FleaScope RS
//! 
//! A Rust library for configuring triggers and communicating with FleaScope devices.
//! 
//! This library provides types and builders for creating digital and analog triggers
//! that can be used to control data capture timing in FleaScope oscilloscope devices,
//! as well as a serial terminal interface for communication.
//! 
//! ## Examples
//! 
//! ### Digital Trigger
//! 
//! ```rust
//! use fleascope_rs::trigger_config::{DigitalTrigger, BitState};
//! 
//! let trigger = DigitalTrigger::start_capturing_when()
//!     .bit0(BitState::High)
//!     .bit1(BitState::Low)
//!     .starts_matching();
//! 
//! let trigger_fields = trigger.into_trigger_fields();
//! println!("Digital trigger: {}", trigger_fields);
//! ```
//! 
//! ### Analog Trigger
//! 
//! ```rust
//! use fleascope_rs::trigger_config::AnalogTrigger;
//! 
//! let trigger = AnalogTrigger::start_capturing_when()
//!     .rising_edge(1.5);
//! 
//! let voltage_to_raw = |v: f64| v * 100.0;
//! let trigger_fields = trigger.into_trigger_fields(voltage_to_raw).unwrap();
//! println!("Analog trigger: {}", trigger_fields);
//! ```
//! 
//! ### Device Connection
//! 
//! ```rust,no_run
//! use fleascope_rs::FleaConnector;
//! 
//! // Connect to any available FleaScope device
//! let terminal = FleaConnector::connect(None, None, true).unwrap();
//! 
//! // Or connect to a specific port
//! let terminal = FleaConnector::connect(None, Some("/dev/ttyUSB0"), true).unwrap();
//! 
//! // List available devices (iterator - memory efficient)
//! let devices = FleaConnector::get_available_devices(None).unwrap();
//! for device in devices.take(3) { // Only process first 3 devices
//!     println!("Found device: {} at {}", device.name, device.port);
//! }
//! 
//! // Or get all devices as a Vec (if you need to access multiple times)
//! let devices_vec = FleaConnector::get_available_devices_vec(None).unwrap();
//! println!("Total devices: {}", devices_vec.len());
//! ```
//! 
//! ### Serial Terminal Communication
//! 
//! ```rust,no_run
//! use fleascope_rs::FleaTerminal;
//! use std::time::Duration;
//! 
//! let mut terminal = FleaTerminal::new("/dev/ttyUSB0")?;
//! terminal.initialize()?;
//! 
//! let response = terminal.exec("version", Some(Duration::from_secs(5)))?;
//! println!("Device version: {}", response);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod trigger_config;
pub mod serial_terminal;
pub mod flea_connector;

// Re-export the main types for convenience
pub use trigger_config::{
    BitState, 
    DigitalTriggerBehavior, 
    AnalogTriggerBehavior,
    BitTriggerBuilder,
    DigitalTrigger,
    AnalogTriggerBuilder,
    AnalogTrigger,
};

pub use serial_terminal::{
    FleaTerminal,
    FleaTerminalError,
};

pub use flea_connector::{
    FleaConnector,
    FleaDevice,
    FleaConnectorError,
};
