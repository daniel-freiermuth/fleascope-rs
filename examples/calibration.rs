// Probe calibration example
//
// This example demonstrates the probe calibration process for both 1x and 10x probes.

use fleascope_rs::{FleaScope, ProbeType};
use std::io::{self, Write};
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    println!("FleaScope Probe Calibration Example");
    println!("===================================\n");

    // Connect to device
    let mut scope = FleaScope::connect(None, None, true)?;
    println!("Connected to FleaScope device\n");

    // Check if we can read some data (indicates calibration might work)
    println!("Checking device status...");
    
    // Try to read a small sample to check if device is responding
    println!("Testing basic data acquisition...");
    match scope.read(ProbeType::X1, Duration::from_millis(1), None, None) {
        Ok(data) => {
            println!("✓ Device is responding, captured {} samples", data.height());
            
            // Try to get a voltage measurement
            let bnc_column = data.column("bnc").unwrap();
            let values = bnc_column.f64().unwrap();
            let first_values: Vec<f64> = values.into_no_null_iter().take(1).collect();
            if let Some(&voltage) = first_values.first() {
                println!("  Current 1x probe reading: {:.3}V", voltage);
            }
        }
        Err(e) => {
            println!("⚠ Warning: Could not read from device: {}", e);
            println!("  This might indicate calibration is needed.");
        }
    }
    
    // Ask user if they want to recalibrate
    print!("\nDo you want to perform new calibration? (y/n): ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    
    if !input.trim().to_lowercase().starts_with('y') {
        println!("Skipping calibration. Using existing values.");
        return Ok(());
    }

    // Calibrate 1x probe
    println!("\n=== 1x Probe Calibration ===");
    calibrate_probe_x1(&mut scope)?;

    // Calibrate 10x probe
    println!("\n=== 10x Probe Calibration ===");
    calibrate_probe_x10(&mut scope)?;

    println!("\n=== Calibration Complete ===");
    
    // Test the calibration by taking a measurement
    println!("Testing calibrated probes...");
    
    // Test 1x probe
    println!("Testing 1x probe:");
    match scope.read(ProbeType::X1, Duration::from_millis(5), None, None) {
        Ok(data) => {
            let bnc_column = data.column("bnc").unwrap();
            let values = bnc_column.f64().unwrap();
            let first_values: Vec<f64> = values.into_no_null_iter().take(1).collect();
            if let Some(&voltage) = first_values.first() {
                println!("  Current measurement: {:.3}V", voltage);
            } else {
                println!("  No measurement data available");
            }
        }
        Err(e) => println!("  Measurement failed: {}", e),
    }
    
    // Test 10x probe
    println!("Testing 10x probe:");
    match scope.read(ProbeType::X10, Duration::from_millis(5), None, None) {
        Ok(data) => {
            let bnc_column = data.column("bnc").unwrap();
            let values = bnc_column.f64().unwrap();
            let first_values: Vec<f64> = values.into_no_null_iter().take(1).collect();
            if let Some(&voltage) = first_values.first() {
                println!("  Current measurement: {:.3}V", voltage);
            } else {
                println!("  No measurement data available");
            }
        }
        Err(e) => println!("  Measurement failed: {}", e),
    }

    // Ask if user wants to save to flash
    print!("\nSave calibration to device flash memory? (y/n): ");
    io::stdout().flush()?;
    input.clear();
    io::stdin().read_line(&mut input)?;
    
    if input.trim().to_lowercase().starts_with('y') {
        scope.write_calibration_to_flash(ProbeType::X1)?;
        scope.write_calibration_to_flash(ProbeType::X10)?;
        println!("Calibration saved to flash memory!");
    } else {
        println!("Calibration not saved. Values will be lost when device is reset.");
    }

    Ok(())
}

fn calibrate_probe_x1(scope: &mut FleaScope) -> Result<(), Box<dyn std::error::Error>> {
    println!("1. Zero calibration for 1x probe");
    println!("   Connect the 1x probe to ground (GND)");
    println!("   Make sure the signal is stable");
    wait_for_user_input("Press Enter when ready...")?;
    
    scope.calibrate_zero(ProbeType::X1)?;
    println!("   ✓ Zero calibration complete");
    
    // Show current measurement to verify it's close to 0V
    match scope.read(ProbeType::X1, Duration::from_millis(5), None, None) {
        Ok(data) => {
            let bnc_column = data.column("bnc").unwrap();
            let values = bnc_column.f64().unwrap();
            let first_values: Vec<f64> = values.into_no_null_iter().take(1).collect();
            if let Some(&voltage) = first_values.first() {
                println!("   Current measurement: {:.3}V (should be close to 0.000V)", voltage);
            } else {
                println!("   No measurement data available");
            }
        }
        Err(e) => println!("   Could not verify measurement: {}", e),
    }

    println!("\n2. Full-scale calibration for 1x probe");
    println!("   Connect the 1x probe to +3.3V");
    println!("   Make sure the signal is stable");
    wait_for_user_input("Press Enter when ready...")?;
    
    scope.calibrate_3v3(ProbeType::X1)?;
    println!("   ✓ Full-scale calibration complete");
    
    // Show current measurement to verify it's close to 3.3V
    match scope.read(ProbeType::X1, Duration::from_millis(5), None, None) {
        Ok(data) => {
            let bnc_column = data.column("bnc").unwrap();
            let values = bnc_column.f64().unwrap();
            let first_values: Vec<f64> = values.into_no_null_iter().take(1).collect();
            if let Some(&voltage) = first_values.first() {
                println!("   Current measurement: {:.3}V (should be close to 3.300V)", voltage);
            } else {
                println!("   No measurement data available");
            }
        }
        Err(e) => println!("   Could not verify measurement: {}", e),
    }
    
    Ok(())
}

fn calibrate_probe_x10(scope: &mut FleaScope) -> Result<(), Box<dyn std::error::Error>> {
    println!("1. Zero calibration for 10x probe");
    println!("   Connect the 10x probe to ground (GND)");
    println!("   Make sure the signal is stable");
    wait_for_user_input("Press Enter when ready...")?;
    
    scope.calibrate_zero(ProbeType::X10)?;
    println!("   ✓ Zero calibration complete");
    
    // Show current measurement to verify it's close to 0V
    match scope.read(ProbeType::X10, Duration::from_millis(5), None, None) {
        Ok(data) => {
            let bnc_column = data.column("bnc").unwrap();
            let values = bnc_column.f64().unwrap();
            let first_values: Vec<f64> = values.into_no_null_iter().take(1).collect();
            if let Some(&voltage) = first_values.first() {
                println!("   Current measurement: {:.3}V (should be close to 0.000V)", voltage);
            } else {
                println!("   No measurement data available");
            }
        }
        Err(e) => println!("   Could not verify measurement: {}", e),
    }

    println!("\n2. Full-scale calibration for 10x probe");
    println!("   Connect the 10x probe to +3.3V");
    println!("   Make sure the signal is stable");
    wait_for_user_input("Press Enter when ready...")?;
    
    scope.calibrate_3v3(ProbeType::X10)?;
    println!("   ✓ Full-scale calibration complete");
    
    // Show current measurement to verify it's close to 3.3V
    match scope.read(ProbeType::X10, Duration::from_millis(5), None, None) {
        Ok(data) => {
            let bnc_column = data.column("bnc").unwrap();
            let values = bnc_column.f64().unwrap();
            let first_values: Vec<f64> = values.into_no_null_iter().take(1).collect();
            if let Some(&voltage) = first_values.first() {
                println!("   Current measurement: {:.3}V (should be close to 3.300V)", voltage);
            } else {
                println!("   No measurement data available");
            }
        }
        Err(e) => println!("   Could not verify measurement: {}", e),
    }
    
    Ok(())
}

fn wait_for_user_input(prompt: &str) -> io::Result<()> {
    print!("{}", prompt);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(())
}
