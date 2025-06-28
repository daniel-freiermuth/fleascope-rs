// Data acquisition with different trigger types
//
// This example demonstrates various trigger configurations and data acquisition methods.

use fleascope_rs::{
    FleaScope, DigitalTrigger, AnalogTrigger, BitState, Waveform
};
use polars::prelude::DataFrame;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    println!("FleaScope Data Acquisition Example");
    println!("==================================\n");

    // Connect to device
    let mut scope = FleaScope::connect(None, None, true)?;
    println!("Connected to FleaScope device\n");

    // Set up a test signal
    scope.set_waveform(Waveform::Sine, 1000)?;
    println!("Generated 1kHz sine wave\n");

    // Example 1: Basic data acquisition with auto trigger
    println!("1. Basic acquisition with auto trigger (1x probe)");
    let data1 = scope.read_x1(Duration::from_millis(10), None, None)?;
    println!("   Captured {} samples over 10ms", data1.height());
    print_data_summary(&data1, "bnc")?;

    // Example 2: Digital trigger
    println!("\n2. Digital trigger acquisition");
    let digital_trigger = DigitalTrigger::start_capturing_when()
        .bit0(BitState::High)     // Trigger when bit 0 is high
        .bit1(BitState::DontCare) // Don't care about bit 1
        .bit2(BitState::Low)      // and bit 2 is low
        .starts_matching();       // Start capturing when pattern matches

    let data2 = scope.read_x1_digital(
        Duration::from_millis(5),
        Some(digital_trigger),
        None,
    )?;
    println!("   Captured {} samples with digital trigger", data2.height());
    print_data_summary(&data2, "bnc")?;

    // Example 3: Analog trigger with rising edge
    println!("\n3. Analog trigger (rising edge at 1.5V)");
    let analog_trigger = AnalogTrigger::start_capturing_when()
        .rising_edge(1.5);

    let data3 = scope.read_x1(
        Duration::from_millis(5),
        Some(analog_trigger),
        Some(Duration::from_micros(100)), // 100μs delay
    )?;
    println!("   Captured {} samples with analog trigger", data3.height());
    print_data_summary(&data3, "bnc")?;

    // Example 4: 10x probe acquisition
    println!("\n4. High voltage acquisition (10x probe)");
    let data4 = scope.read_x10(Duration::from_millis(8), None, None)?;
    println!("   Captured {} samples with 10x probe", data4.height());
    print_data_summary(&data4, "bnc")?;

    // Example 5: Digital bit analysis
    println!("\n5. Digital bit analysis");
    let digital_data = scope.read_x1_digital(
        Duration::from_millis(3),
        None,
        None,
    )?;
    
    // Extract individual bits from the bitmap
    let bits_data = FleaScope::extract_bits(&digital_data)?;
    println!("   Original data: {} samples", digital_data.height());
    println!("   Extracted bits: {} columns", bits_data.width());
    
    // Show column names
    let columns: Vec<_> = bits_data.get_column_names();
    println!("   Columns: {:?}", columns);

    println!("\nData acquisition examples completed!");
    Ok(())
}

// Helper function to print basic statistics about acquired data
fn print_data_summary(data: &DataFrame, column: &str) -> Result<(), Box<dyn std::error::Error>> {
    
    let col = data.column(column)?;
    
    if let Ok(series) = col.f64() {
        let values: Vec<f64> = series.into_no_null_iter().collect();
        
        if !values.is_empty() {
            let min = values.iter().fold(f64::INFINITY, |a, &b| a.min(b));
            let max = values.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            
            println!("   Data range: {:.3}V to {:.3}V (mean: {:.3}V)", min, max, mean);
        }
    }
    
    Ok(())
}
