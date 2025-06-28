# Data Acquisition Example Updates

## Summary of Changes

The data acquisition example has been comprehensively updated to use the new unified trigger API and ProbeType-based methods, replacing the old duplicated methods that were removed during the API redesign.

## Key Updates

### 1. Import Changes
**Before:**
```rust
use fleascope_rs::{
    FleaScope, DigitalTrigger, AnalogTrigger, BitState, Waveform
};
```

**After:**
```rust
use fleascope_rs::{
    FleaScope, ProbeType, DigitalTrigger, AnalogTrigger, Trigger, BitState, Waveform
};
```

Added imports for:
- `ProbeType`: For specifying 1x or 10x probe types
- `Trigger`: For the unified trigger API

### 2. Method Call Updates

#### Basic Data Acquisition
**Before:**
```rust
let data1 = scope.read_x1(Duration::from_millis(10), None, None)?;
```

**After:**
```rust
let data1 = scope.read(ProbeType::X1, Duration::from_millis(10), None, None)?;
```

#### Digital Trigger Acquisition
**Before:**
```rust
let data2 = scope.read_x1_digital(
    Duration::from_millis(5),
    Some(digital_trigger),
    None,
)?;
```

**After:**
```rust
let data2 = scope.read(
    ProbeType::X1,
    Duration::from_millis(5),
    Some(Trigger::from(digital_trigger)), // Unified trigger API
    None,
)?;
```

#### Analog Trigger Acquisition
**Before:**
```rust
let data3 = scope.read_x1(
    Duration::from_millis(5),
    Some(analog_trigger),
    Some(Duration::from_micros(100)),
)?;
```

**After:**
```rust
let data3 = scope.read(
    ProbeType::X1,
    Duration::from_millis(5),
    Some(Trigger::from(analog_trigger)),
    Some(Duration::from_micros(100)),
)?;
```

#### 10x Probe Acquisition
**Before:**
```rust
let data4 = scope.read_x10(Duration::from_millis(8), None, None)?;
```

**After:**
```rust
let data4 = scope.read(ProbeType::X10, Duration::from_millis(8), None, None)?;
```

### 3. New Examples Added

#### Unified API Flexibility Demonstration
Added a comprehensive example showing how the unified trigger API allows mixing different trigger types with different probe types:

```rust
// Example 6: Unified API flexibility demonstration
let triggers: Vec<(Trigger, &str)> = vec![
    (Trigger::from(AnalogTrigger::start_capturing_when().auto(0.0)), "Auto analog"),
    (Trigger::from(AnalogTrigger::start_capturing_when().rising_edge(2.0)), "Rising edge at 2V"),
    (Trigger::from(DigitalTrigger::start_capturing_when().bit0(BitState::High).is_matching()), "Digital bit 0 high"),
];

// Test each trigger with both probe types
for (probe_type, probe_name) in [(ProbeType::X1, "1x"), (ProbeType::X10, "10x")] {
    for (trigger, trigger_name) in &triggers {
        let data = scope.read(
            probe_type,
            Duration::from_millis(2),
            Some(trigger.clone()),
            None,
        )?;
        println!("   {} probe with {} trigger: {} samples", 
                probe_name, trigger_name, data.height());
    }
}
```

## Supporting Changes

### Calibration Example Updates
Updated the calibration example to use new getter methods instead of accessing private fields:

**Before:**
```rust
let (x1_zero, x1_3v3) = scope.x1.calibration();
let (x10_zero, x10_3v3) = scope.x10.calibration();
```

**After:**
```rust
let (x1_zero, x1_3v3) = scope.get_x1_calibration();
let (x10_zero, x10_3v3) = scope.get_x10_calibration();
```

### New Getter Methods in FleaScope
Added public getter methods for accessing probe calibration:

```rust
impl FleaScope {
    /// Get calibration values for a specific probe
    pub fn get_probe_calibration(&self, probe: ProbeType) -> (Option<f64>, Option<f64>) {
        self.get_probe(probe).calibration()
    }

    /// Get calibration values for the 1x probe
    pub fn get_x1_calibration(&self) -> (Option<f64>, Option<f64>) {
        self.x1.calibration()
    }

    /// Get calibration values for the 10x probe  
    pub fn get_x10_calibration(&self) -> (Option<f64>, Option<f64>) {
        self.x10.calibration()
    }
}
```

## Benefits of Updated Example

1. **API Consistency**: All examples now use the same unified `read()` method
2. **Type Safety**: ProbeType enum ensures compile-time correctness
3. **Flexibility**: Demonstrates how any trigger can work with any probe type
4. **Educational Value**: Shows both the power and simplicity of the unified API
5. **Real-world Usage**: Provides practical examples for different use cases

## Testing Results

- ✅ All examples compile successfully
- ✅ All unit tests pass (13/13)
- ✅ All documentation tests pass (6/6)
- ✅ No clippy warnings (except for one unrelated unused field warning)

The updated data acquisition example now perfectly demonstrates the ergonomic, unified API while providing comprehensive coverage of different trigger types and probe configurations.
