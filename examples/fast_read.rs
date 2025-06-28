// Fast data reading example
//
// This example demonstrates high-speed data acquisition from the FleaScope
// using digital triggers and continuous reading loops.

use fleascope_rs::{FleaScope, ProbeType, DigitalTrigger, Trigger};
use clap::Parser;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "fast_read")]
#[command(author = "FleaScope Team")]
#[command(version = "1.0")]
#[command(about = "High-speed data acquisition from FleaScope")]
#[command(long_about = "Continuously read data from a FleaScope device as fast as possible using digital triggers. Great for performance testing and real-time monitoring.")]
struct Args {
    /// Device name to connect to
    device_name: String,
    
    /// Time frame in milliseconds
    #[arg(short, long, default_value_t = 70, help = "Duration of each data capture in milliseconds")]
    time_frame: u64,
    
    /// Probe type to use
    #[arg(short, long, default_value = "x1", value_parser = ["x1", "x10", "1", "10"], help = "Probe multiplier (x1 or x10)")]
    probe: String,
    
    /// Enable verbose logging
    #[arg(short, long, help = "Show debug information and detailed logs")]
    verbose: bool,
    
    /// Display only statistics (no voltage values)
    #[arg(short, long, help = "Show only performance statistics, not voltage readings")]
    stats_only: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    
    if args.verbose {
        env_logger::Builder::from_default_env()
            .filter_level(log::LevelFilter::Debug)
            .init();
    } else {
        env_logger::init();
    }

    // Parse probe type
    let probe = match args.probe.to_lowercase().as_str() {
        "x1" | "1" => ProbeType::X1,
        "x10" | "10" => ProbeType::X10,
        _ => {
            eprintln!("Invalid probe type: {}. Use 'x1' or 'x10'", args.probe);
            std::process::exit(1);
        }
    };

    println!("FleaScope Fast Data Reader");
    println!("=========================");
    println!("Device: {}", args.device_name);
    println!("Time frame: {}ms", args.time_frame);
    println!("Probe: {} ({}x)", args.probe.to_uppercase(), if probe == ProbeType::X1 { 1 } else { 10 });
    println!("Trigger: Digital (immediate capture)");
    println!("Press Ctrl+C to stop\n");

    // Connect to the specific device
    let mut scope = FleaScope::connect(Some(&args.device_name), None, true)?;
    println!("✓ Connected to device: {}", args.device_name);

    // Set up parameters
    let time_frame = Duration::from_millis(args.time_frame);
    let trigger: Trigger = DigitalTrigger::start_capturing_when().is_matching().into();

    println!("Starting continuous data acquisition...\n");

    let mut sample_count = 0u64;
    let start_time = std::time::Instant::now();

    loop {
        match scope.read(probe, time_frame, Some(trigger.clone()), None) {
            Ok(data) => {
                sample_count += 1;
                let num_samples = data.height();
                let elapsed = start_time.elapsed();
                
                // Calculate statistics
                let samples_per_sec = sample_count as f64 / elapsed.as_secs_f64();
                let data_points_per_sec = (sample_count * num_samples as u64) as f64 / elapsed.as_secs_f64();
                
                // Get some sample data points
                if let Ok(bnc_column) = data.column("bnc") {
                    if let Ok(values) = bnc_column.f64() {
                        if args.stats_only {
                            // Show only statistics
                            print!("\r[{}] {} samples ({} points) | {:.1} Hz | {:.0} pts/s", 
                                   format_duration(elapsed),
                                   sample_count, 
                                   num_samples,
                                   samples_per_sec,
                                   data_points_per_sec);
                        } else {
                            // Show statistics and voltage samples
                            let first_values: Vec<f64> = values.into_no_null_iter().take(5).collect();
                            let last_values: Vec<f64> = values.into_no_null_iter().rev().take(5).collect();
                            
                            // Print status
                            print!("\r[{}] {} samples ({} points) | {:.1} Hz | {:.0} pts/s | First: [", 
                                   format_duration(elapsed),
                                   sample_count, 
                                   num_samples,
                                   samples_per_sec,
                                   data_points_per_sec);
                            
                            for (i, &val) in first_values.iter().enumerate() {
                                if i > 0 { print!(", "); }
                                print!("{:.3}V", val);
                            }
                            
                            print!("] Last: [");
                            for (i, &val) in last_values.iter().rev().enumerate() {
                                if i > 0 { print!(", "); }
                                print!("{:.3}V", val);
                            }
                            print!("]");
                        }
                        
                        use std::io::{self, Write};
                        io::stdout().flush()?;
                    }
                }
            }
            Err(e) => {
                eprintln!("\nError reading data: {}", e);
                eprintln!("Retrying in 100ms...");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn format_duration(duration: Duration) -> String {
    let total_secs = duration.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    
    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
    } else {
        format!("{:02}:{:02}", minutes, seconds)
    }
}
