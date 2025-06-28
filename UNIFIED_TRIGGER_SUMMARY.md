# Unified Trigger API Summary

This document summarizes the implementation of a unified trigger API that allows treating digital and analog triggers similarly in the FleaScope Rust library.

## Problem Statement

Previously, the API required users to call different methods for analog and digital triggers:
- `scope.read()` for analog triggers
- `scope.read_digital()` for digital triggers

This created API inconsistency and forced users to know the trigger type at compile time.

## Solution: Unified Trigger Enum

We implemented a unified `Trigger` enum that can represent both analog and digital triggers:

```rust
#[derive(Debug, Clone)]
pub enum Trigger {
    Analog(AnalogTrigger),
    Digital(DigitalTrigger),
}
```

### Key Features

1. **Type Safety**: The enum ensures only valid trigger types can be passed.
2. **Ergonomic Conversions**: Automatic `From` trait implementations for easy conversion.
3. **Unified API**: Single `read()` method accepts any trigger type.
4. **Backward Compatibility**: Legacy methods (`read_analog`, `read_digital`) still available.

## API Changes

### New Unified API

```rust
// Create triggers
let analog_trigger = AnalogTrigger::start_capturing_when().rising_edge(1.5);
let digital_trigger = DigitalTrigger::start_capturing_when().bit0(BitState::High).is_matching();

// Use unified API
let data = scope.read(ProbeType::X1, Duration::from_millis(5), Some(Trigger::from(analog_trigger)), None)?;
let data = scope.read(ProbeType::X1, Duration::from_millis(5), Some(Trigger::from(digital_trigger)), None)?;

// Even simpler with automatic conversion
let data = scope.read(ProbeType::X1, Duration::from_millis(5), Some(analog_trigger.into()), None)?;
```

### Legacy API (Still Supported)

```rust
// Analog trigger (legacy)
let data = scope.read_analog(ProbeType::X1, Duration::from_millis(5), Some(analog_trigger), None)?;

// Digital trigger (legacy)  
let data = scope.read_digital(ProbeType::X1, Duration::from_millis(5), Some(digital_trigger), None)?;
```

## Implementation Details

### 1. Added Clone Trait

Both `AnalogTrigger` and `DigitalTrigger` now implement `Clone`:

```rust
#[derive(Debug, Clone)]
pub struct AnalogTrigger { /* ... */ }

#[derive(Debug, Clone)]
pub struct DigitalTrigger { /* ... */ }
```

### 2. Unified Trigger Enum

```rust
#[derive(Debug, Clone)]
pub enum Trigger {
    Analog(AnalogTrigger),
    Digital(DigitalTrigger),
}

impl From<AnalogTrigger> for Trigger {
    fn from(trigger: AnalogTrigger) -> Self {
        Self::Analog(trigger)
    }
}

impl From<DigitalTrigger> for Trigger {
    fn from(trigger: DigitalTrigger) -> Self {
        Self::Digital(trigger)
    }
}
```

### 3. Unified Read Method

The main `read` method now accepts the unified trigger:

```rust
pub fn read(
    &mut self,
    probe: ProbeType,
    time_frame: Duration,
    trigger: Option<Trigger>,
    delay: Option<Duration>,
) -> Result<DataFrame, FleaScopeError> {
    let trigger_fields = if let Some(trigger) = trigger {
        match trigger {
            Trigger::Analog(analog_trigger) => {
                let probe_ref = self.get_probe(probe);
                analog_trigger
                    .into_trigger_fields(|v| probe_ref.voltage_to_raw(v).unwrap_or(0.0))
                    .map_err(|_| FleaScopeError::CalibrationNotSet)?
            }
            Trigger::Digital(digital_trigger) => digital_trigger.into_trigger_fields(),
        }
    } else {
        // Default to analog auto trigger at 0V
        let probe_ref = self.get_probe(probe);
        AnalogTrigger::start_capturing_when()
            .auto(0.0)
            .into_trigger_fields(|v| probe_ref.voltage_to_raw(v).unwrap_or(0.0))
            .map_err(|_| FleaScopeError::CalibrationNotSet)?
    };
    
    // ... rest of implementation
}
```

### 4. Probe Methods Updated

FleaProbe methods also support the unified API:

```rust
impl FleaProbe {
    pub fn read(
        &self,
        scope: &mut FleaScope,
        time_frame: Duration,
        trigger: Option<Trigger>,
        delay: Option<Duration>,
    ) -> Result<DataFrame, FleaScopeError> {
        // Unified implementation
    }
}
```

## Benefits

1. **Consistency**: Single method for all trigger types
2. **Flexibility**: Users can switch trigger types without changing method calls
3. **Type Safety**: Compile-time guarantee of valid trigger types
4. **Ergonomics**: Easy conversion from specific trigger types to unified type
5. **Backward Compatibility**: Existing code continues to work
6. **Future-Proof**: Easy to add new trigger types without API changes

## Migration Guide

### For New Code
Use the unified API:
```rust
let trigger = AnalogTrigger::start_capturing_when().rising_edge(2.0);
let data = scope.read(ProbeType::X1, time_frame, Some(trigger.into()), None)?;
```

### For Existing Code
No changes required! Legacy methods still work:
```rust
// This continues to work
let data = scope.read_analog(ProbeType::X1, time_frame, Some(trigger), None)?;
```

## Testing

- All existing tests pass
- New unified trigger tests added
- Examples updated to demonstrate both APIs
- Documentation examples compile and run
- Clippy passes with no warnings

## Future Enhancements

The unified trigger API provides a foundation for:
1. **Trigger Composition**: Combining multiple triggers
2. **Trigger Presets**: Common trigger configurations as constants
3. **Dynamic Trigger Selection**: Runtime trigger type determination
4. **Advanced Triggers**: New trigger types (e.g., complex patterns, time-based triggers)

This implementation successfully unifies digital and analog trigger handling while maintaining full backward compatibility and providing an ergonomic, type-safe API.
