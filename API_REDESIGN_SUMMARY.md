# FleaScope API Redesign: ProbeType Enum Solution

## Problem Solved

The previous API had a **fundamental architectural flaw**: probe instance methods needed mutable access to their owning scope, creating borrowing conflicts that required error-prone workarounds like `std::mem::take`.

### Before (Problematic):
```rust
// Required complex borrowing workarounds
let data = {
    let probe = std::mem::take(&mut scope.x1);
    let result = probe.read(&mut scope, Duration::from_millis(10), None, None);
    scope.x1 = probe;
    result?
};

// Or duplicated methods leading to API explosion
scope.read_x1(...)  // 4 read methods
scope.read_x10(...) // + 4 calibration methods  
scope.calibrate_x1_zero() // + 2 flash methods
scope.calibrate_x1_3v3()   // = 10+ methods total
// ... and growing with each new probe type
```

## Solution: ProbeType Enum

### New Clean API:
```rust
use fleascope_rs::{FleaScope, ProbeType};

// Data acquisition - no borrowing issues!
let data = scope.read(ProbeType::X1, Duration::from_millis(10), None, None)?;
let data = scope.read(ProbeType::X10, Duration::from_millis(10), None, None)?;
let data = scope.read_digital(ProbeType::X1, Duration::from_millis(5), trigger, None)?;

// Calibration - simple and safe
scope.calibrate_zero(ProbeType::X1)?;
scope.calibrate_3v3(ProbeType::X1)?;
scope.write_calibration_to_flash(ProbeType::X10)?;
```

## Benefits

### 1. **No Borrowing Issues**
- Scope controls its own state
- No circular dependency between probe and scope
- No need for `std::mem::take` workarounds

### 2. **Scalable API**
- Only 5 core methods regardless of probe count
- Adding new probe types requires one enum variant
- No combinatorial explosion of methods

### 3. **Type Safety**
- `ProbeType` is `Copy` - no ownership issues
- Compile-time validation of probe selection
- Clear, readable code

### 4. **Consistent Patterns**
- All operations follow the same `scope.operation(probe_type, ...)` pattern
- Uniform error handling
- Predictable API surface

## Implementation Details

### Core Methods:
```rust
impl FleaScope {
    pub fn read(&mut self, probe: ProbeType, time_frame: Duration, trigger: Option<AnalogTrigger>, delay: Option<Duration>) -> Result<DataFrame, FleaScopeError>
    
    pub fn read_digital(&mut self, probe: ProbeType, time_frame: Duration, trigger: Option<DigitalTrigger>, delay: Option<Duration>) -> Result<DataFrame, FleaScopeError>
    
    pub fn calibrate_zero(&mut self, probe: ProbeType) -> Result<f64, FleaScopeError>
    
    pub fn calibrate_3v3(&mut self, probe: ProbeType) -> Result<f64, FleaScopeError>
    
    pub fn write_calibration_to_flash(&mut self, probe: ProbeType) -> Result<(), FleaScopeError>
}
```

### ProbeType Definition:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeType {
    X1,
    X10,
}
```

## Migration Guide

| Old API | New API |
|---------|---------|
| `scope.read_x1(...)` | `scope.read(ProbeType::X1, ...)` |
| `scope.read_x10(...)` | `scope.read(ProbeType::X10, ...)` |
| `scope.read_x1_digital(...)` | `scope.read_digital(ProbeType::X1, ...)` |
| `scope.calibrate_x1_zero()` | `scope.calibrate_zero(ProbeType::X1)` |
| Complex `std::mem::take` patterns | Simple direct calls |

## Conclusion

This redesign eliminates the architectural anti-pattern while providing a clean, scalable API that:
- Works naturally with Rust's ownership system
- Scales to any number of probe types
- Maintains type safety and performance
- Dramatically simplifies both implementation and usage

The API is now **comfortable, ergonomic, and maintainable** - exactly what we wanted!
