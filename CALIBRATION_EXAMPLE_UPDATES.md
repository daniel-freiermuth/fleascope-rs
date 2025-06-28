# Calibration Example Updates

## Summary of Changes

The calibration example has been completely rewritten to show measured voltage values instead of raw calibration data, providing a much more user-friendly interface that doesn't expose internal implementation details.

## Key Improvements

### 1. User-Friendly Display
**Before (showing raw calibration values):**
```
Current 1x probe calibration: zero=Some(2048.3), 3v3=Some(991.7)
Current 10x probe calibration: zero=Some(2051.1), 3v3=Some(99.2)
```

**After (showing calibration status and measurements):**
```
1x probe calibration status: ✓ Calibrated
10x probe calibration status: ✓ Calibrated
Current 1x probe measurement: 1.650V
Current 10x probe measurement: 16.500V
```

### 2. New API Methods Added

#### Voltage Measurement Method
```rust
/// Get current measured voltage from a probe (single sample)
pub fn measure_voltage(&mut self, probe: ProbeType) -> Result<f64, FleaScopeError> {
    // Takes a quick 1ms measurement and returns the first voltage sample
    let data = self.read(probe, Duration::from_millis(1), None, None)?;
    let bnc_column = data.column("bnc")?;
    let values = bnc_column.f64()?;
    let first_values: Vec<f64> = values.into_no_null_iter().take(1).collect();
    
    if let Some(&voltage) = first_values.first() {
        Ok(voltage)
    } else {
        Err(FleaScopeError::DataParsing("No voltage data available".to_string()))
    }
}
```

#### Calibration Status Check
```rust
/// Check if a probe is calibrated (has both zero and 3.3V calibration values)
pub fn is_probe_calibrated(&self, probe: ProbeType) -> bool {
    let (zero, v3v3) = self.get_probe_calibration(probe);
    zero.is_some() && v3v3.is_some()
}
```

### 3. Enhanced Calibration Process

#### Real-time Feedback During Calibration
**Before:**
```
✓ Zero calibration complete: 2048.30
✓ Full-scale calibration complete: 991.70
```

**After:**
```
✓ Zero calibration complete
Current measurement: 0.003V (should be close to 0.000V)

✓ Full-scale calibration complete  
Current measurement: 3.297V (should be close to 3.300V)
```

#### Status-Based Display
Shows clear calibration status with checkmarks and current voltage readings:

```
Final calibration status:
  1x probe: ✓ Calibrated
  10x probe: ✓ Calibrated
  1x probe current measurement: 3.297V
  10x probe current measurement: 3.298V
```

## Benefits of the New Approach

### 1. **No Raw Data Exposure**
- Hides internal calibration values from the user
- Shows meaningful voltage measurements instead
- More intuitive and user-friendly interface

### 2. **Real-time Verification**
- Shows actual measured voltages during calibration
- Allows users to verify calibration accuracy immediately
- Provides visual feedback that calibration worked correctly

### 3. **Clear Status Indication**
- Uses ✓/✗ symbols for clear visual feedback
- Shows calibration status before starting
- Displays final calibration results with measurements

### 4. **Better Error Handling**
- Graceful error messages if measurements fail
- Non-blocking error display (continues with other probes)
- Clear indication of what went wrong

## Example Output Flow

### Initial Status Check
```
Checking existing calibration...
1x probe calibration status: ✗ Not calibrated
10x probe calibration status: ✓ Calibrated
Current 10x probe measurement: 1.650V
```

### During Calibration
```
=== 1x Probe Calibration ===
1. Zero calibration for 1x probe
   Connect the 1x probe to ground (GND)
   Make sure the signal is stable
Press Enter when ready...
   ✓ Zero calibration complete
   Current measurement: 0.003V (should be close to 0.000V)

2. Full-scale calibration for 1x probe
   Connect the 1x probe to +3.3V
   Make sure the signal is stable
Press Enter when ready...
   ✓ Full-scale calibration complete
   Current measurement: 3.297V (should be close to 3.300V)
```

### Final Results
```
=== Calibration Complete ===
Final calibration status:
  1x probe: ✓ Calibrated
  10x probe: ✓ Calibrated
  1x probe current measurement: 3.297V
  10x probe current measurement: 3.298V
```

## API Design Benefits

1. **Encapsulation**: Raw calibration values are kept internal to the library
2. **User-Focused**: API exposes what users care about (voltages, not raw ADC values)
3. **Immediate Feedback**: Users can see if their calibration setup is working
4. **Error Tolerance**: Continues operation even if some measurements fail
5. **Professional UX**: Clean, status-oriented display suitable for production tools

This rewrite transforms a developer-focused calibration tool into a user-friendly instrument calibration interface that could be used in production environments.
