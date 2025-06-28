// Probe calibration example
//
// This example demonstrates the probe calibration process for both 1x and 10x probes.

use fleascope_rs::FleaScope;
use std::io::{self, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    println!("FleaScope Probe Calibration Example");
    println!("===================================\n");

    // Connect to device
    let mut scope = FleaScope::connect(None, None, true)?;
    println!("Connected to FleaScope device\n");

    // Check if calibration already exists
    println!("Checking existing calibration...");
    
    let (x1_zero, x1_3v3) = scope.x1.calibration();
    let (x10_zero, x10_3v3) = scope.x10.calibration();
    
    println!("Current 1x probe calibration: zero={:?}, 3v3={:?}", x1_zero, x1_3v3);
    println!("Current 10x probe calibration: zero={:?}, 3v3={:?}", x10_zero, x10_3v3);
    
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
    let (x1_zero, x1_3v3) = scope.x1.calibration();
    let (x10_zero, x10_3v3) = scope.x10.calibration();
    
    println!("Final 1x probe calibration: zero={:?}, 3v3={:?}", x1_zero, x1_3v3);
    println!("Final 10x probe calibration: zero={:?}, 3v3={:?}", x10_zero, x10_3v3);

    // Ask if user wants to save to flash
    print!("\nSave calibration to device flash memory? (y/n): ");
    io::stdout().flush()?;
    input.clear();
    io::stdin().read_line(&mut input)?;
    
    if input.trim().to_lowercase().starts_with('y') {
        scope.write_x1_calibration_to_flash()?;
        scope.write_x10_calibration_to_flash()?;
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
    
    let zero_value = scope.calibrate_x1_zero()?;
    println!("   ✓ Zero calibration complete: {:.2}", zero_value);

    println!("\n2. Full-scale calibration for 1x probe");
    println!("   Connect the 1x probe to +3.3V");
    println!("   Make sure the signal is stable");
    wait_for_user_input("Press Enter when ready...")?;
    
    let full_scale = scope.calibrate_x1_3v3()?;
    println!("   ✓ Full-scale calibration complete: {:.2}", full_scale);
    
    Ok(())
}

fn calibrate_probe_x10(scope: &mut FleaScope) -> Result<(), Box<dyn std::error::Error>> {
    println!("1. Zero calibration for 10x probe");
    println!("   Connect the 10x probe to ground (GND)");
    println!("   Make sure the signal is stable");
    wait_for_user_input("Press Enter when ready...")?;
    
    let zero_value = scope.calibrate_x10_zero()?;
    println!("   ✓ Zero calibration complete: {:.2}", zero_value);

    println!("\n2. Full-scale calibration for 10x probe");
    println!("   Connect the 10x probe to +3.3V");
    println!("   Make sure the signal is stable");
    wait_for_user_input("Press Enter when ready...")?;
    
    let full_scale = scope.calibrate_x10_3v3()?;
    println!("   ✓ Full-scale calibration complete: {:.2}", full_scale);
    
    Ok(())
}

fn wait_for_user_input(prompt: &str) -> io::Result<()> {
    print!("{}", prompt);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(())
}
