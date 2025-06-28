// Basic device discovery and connection example
//
// This example shows how to discover FleaScope devices and establish a basic connection.

use fleascope_rs::{FleaConnector, FleaScope};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging (optional)
    env_logger::init();

    println!("FleaScope Device Discovery Example");
    println!("=================================\n");

    // Method 1: List all available devices
    println!("1. Discovering available FleaScope devices...");
    let devices = FleaConnector::get_available_devices_vec(None)?;
    
    if devices.is_empty() {
        println!("No FleaScope devices found. Please connect a device and try again.");
        return Ok(());
    }

    println!("Found {} device(s):", devices.len());
    for (i, device) in devices.iter().enumerate() {
        println!("  {}. {} at {}", i + 1, device.name, device.port);
    }
    println!();

    // Method 2: Connect to the first available device
    println!("2. Connecting to first available device...");
    let mut scope = FleaScope::connect(None, None, true)?;
    println!("Successfully connected!");

    // Method 3: Basic device information
    println!("\n3. Device information:");
    // Note: In a real implementation, you might want to add methods to get device info
    println!("Connection established with auto-initialization");

    // Method 4: Test basic communication
    println!("\n4. Testing basic communication...");
    // Set a simple waveform to test communication
    scope.set_waveform(fleascope_rs::Waveform::Sine, 100)?;
    println!("Successfully set 100Hz sine wave");

    println!("\n5. Connection test completed successfully!");
    
    Ok(())
}
