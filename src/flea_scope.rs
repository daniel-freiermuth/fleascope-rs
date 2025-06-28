use crate::flea_connector::{FleaConnector, FleaConnectorError};
use crate::serial_terminal::{FleaTerminal, FleaTerminalError};
use crate::trigger_config::{AnalogTrigger, DigitalTrigger, Trigger};
use polars::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeType {
    X1,
    X10,
}

#[derive(Debug, Clone)]
pub enum Waveform {
    Sine,
    Square,
    Triangle,
    Ekg,
}

impl Waveform {
    pub fn as_str(&self) -> &'static str {
        match self {
            Waveform::Sine => "sine",
            Waveform::Square => "square",
            Waveform::Triangle => "triangle",
            Waveform::Ekg => "ekg",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FleaScopeError {
    #[error("Serial terminal error: {0}")]
    SerialTerminal(#[from] FleaTerminalError),

    #[error("Connector error: {0}")]
    Connector(#[from] FleaConnectorError),

    #[error("Polars error: {0}")]
    Polars(#[from] PolarsError),

    #[error("Time frame cannot be negative")]
    NegativeTimeFrame,

    #[error("Time frame too large (max 3.49 seconds)")]
    TimeFrameTooLarge,

    #[error("Time frame too small (min 111 microseconds)")]
    TimeFrameTooSmall,

    #[error("Delay cannot be negative")]
    NegativeDelay,

    #[error("Delay too large (max 1 second)")]
    DelayTooLarge,

    #[error("Delay samples too large (max 1M samples): {samples}")]
    DelaySamplesTooLarge { samples: i32 },

    #[error("Ticks per sample must be greater than 0")]
    InvalidTicksPerSample,

    #[error("Prescaler must be greater than 0")]
    InvalidPrescalerLow,

    #[error("Prescaler must be less than 65536")]
    InvalidPrescalerHigh,

    #[error("Calibration values are not set")]
    CalibrationNotSet,

    #[error("Calibration values for probe x{multiplier} are equal ({value})")]
    CalibrationValuesEqual { multiplier: i32, value: f64 },

    #[error("Zero-Calibration needs to be done first")]
    ZeroCalibrationRequired,

    #[error("Signal is not stable enough for calibration. Values ranged from {min} to {max}")]
    SignalNotStable { min: f64, max: f64 },

    #[error("Data parsing error: {0}")]
    DataParsing(String),
}

pub struct FleaScope {
    serial: FleaTerminal,
    _ver: String,
    hostname: String,
    x1: FleaProbe,
    x10: FleaProbe,
}

impl FleaScope {
    // Constants
    const MSPS: f64 = 18.0; // Million samples per second. target sample rate
    const MCU_MHZ: f64 = 120.0; // MCU clock frequency in MHz, used for calculations
    const INTERLEAVE: f64 = 5.0; // number of ADCs interleaved
    const TOTAL_SAMPLES: f64 = 2000.0;

    /// Connect to a FleaScope device
    pub fn connect(
        name: Option<&str>,
        port: Option<&str>,
        read_calibrations: bool,
    ) -> Result<Self, FleaScopeError> {
        let serial = FleaConnector::connect(name, port, true)?;
        Self::new(serial, read_calibrations)
    }

    /// Create a new FleaScope from an existing terminal connection
    pub fn new(mut serial: FleaTerminal, read_calibrations: bool) -> Result<Self, FleaScopeError> {
        log::debug!("Turning off echo");
        serial.exec("echo off", None)?;

        // TODO: try to gear up to 115200 baud

        let ver = serial.exec("ver", None)?;
        log::debug!("FleaScope version: {}", ver);
        // TODO: check if version is compatible

        let hostname = serial.exec("hostname", None)?;
        log::debug!("FleaScope hostname: {}", hostname);
        // TODO: check if hostname is correct

        let x1 = FleaProbe::new(1);
        let x10 = FleaProbe::new(10);

        let mut scope = Self {
            serial,
            _ver: ver,
            hostname,
            x1,
            x10,
        };

        if read_calibrations {
            scope.x1.read_calibration_from_flash(&mut scope.serial)?;
            scope.x10.read_calibration_from_flash(&mut scope.serial)?;
        }

        Ok(scope)
    }

    /// Read data with unified trigger (supports both analog and digital triggers)
    pub fn read(
        &mut self,
        probe: ProbeType,
        time_frame: Duration,
        trigger: Option<Trigger>,
        delay: Option<Duration>,
    ) -> Result<LazyFrame, FleaScopeError> {
        let trigger_fields = if let Some(trigger) = trigger {
            match trigger {
                Trigger::Analog(analog_trigger) => {
                    let probe_ref = self.get_probe(probe);
                    analog_trigger
                        .into_trigger_fields(|v| probe_ref.voltage_to_raw(v).unwrap())
                        .map_err(|_| FleaScopeError::CalibrationNotSet)?
                }
                Trigger::Digital(digital_trigger) => digital_trigger.into_trigger_fields(),
            }
        } else {
            // Default to analog auto trigger at 0V
            let probe_ref = self.get_probe(probe);
            AnalogTrigger::start_capturing_when()
                .auto(0.0)
                .into_trigger_fields(|v| probe_ref.voltage_to_raw(v).unwrap())
                .map_err(|_| FleaScopeError::CalibrationNotSet)?
        };

        let df = self.raw_read(time_frame, &trigger_fields, delay)?;

        // Convert BNC values from raw to voltage
        let probe_ref = self.get_probe(probe);
        let res = df.select([
            col("time"),
            probe_ref.raw_to_voltage(col("bnc"))?,
            col("bitmap"),
        ]);

        Ok(res)
    }


    /// Set the waveform generator
    pub fn set_waveform(&mut self, waveform: Waveform, hz: i32) -> Result<(), FleaScopeError> {
        self.serial
            .exec(&format!("wave {} {}", waveform.as_str(), hz), None)?;
        Ok(())
    }

    /// Convert Duration to microseconds
    fn duration_to_us(duration: Duration) -> u64 {
        duration.as_micros() as u64
    }

    /// Convert number1 to prescaler value
    fn number1_to_prescaler(number1: i32) -> Result<i32, FleaScopeError> {
        let ps = if number1 > 1000 { 16 } else { 1 };
        let t = ((Self::MCU_MHZ * number1 as f64 * Self::INTERLEAVE / ps as f64 / Self::MSPS) + 0.5)
            as i32;

        if t <= 0 {
            return Err(FleaScopeError::InvalidPrescalerLow);
        }
        if t > 65535 {
            return Err(FleaScopeError::InvalidPrescalerHigh);
        }

        Ok(ps * t)
    }

    /// Convert prescaler to effective MSPS
    fn prescaler_to_effective_msps(prescaler: i32) -> f64 {
        Self::MCU_MHZ * Self::INTERLEAVE / prescaler as f64
    }

    /// Raw data read from the oscilloscope
    pub fn raw_read(
        &mut self,
        time_frame: Duration,
        trigger_fields: &str,
        delay: Option<Duration>,
    ) -> Result<LazyFrame, FleaScopeError> {
        let delay = delay.unwrap_or(Duration::from_millis(0));

        // Validate time frame
        if time_frame.as_secs_f64() < 0.0 {
            return Err(FleaScopeError::NegativeTimeFrame);
        }
        if time_frame.as_secs_f64() > 3.49 {
            return Err(FleaScopeError::TimeFrameTooLarge);
        }
        if time_frame.as_secs() == 0 && time_frame.as_micros() < 111 {
            return Err(FleaScopeError::TimeFrameTooSmall);
        }

        // Validate delay
        if delay.as_secs_f64() < 0.0 {
            return Err(FleaScopeError::NegativeDelay);
        }
        if delay.as_secs_f64() > 1.0 {
            return Err(FleaScopeError::DelayTooLarge);
        }

        let number1 = (Self::MSPS * Self::duration_to_us(time_frame) as f64
            / Self::TOTAL_SAMPLES) as i32;
        if number1 <= 0 {
            return Err(FleaScopeError::InvalidTicksPerSample);
        }

        let prescaler = Self::number1_to_prescaler(number1)?;
        let effective_msps = Self::prescaler_to_effective_msps(prescaler);

        let delay_samples =
            (Self::duration_to_us(delay) as f64 * effective_msps / 1_000_000.0) as i32;
        if delay_samples > 1_000_000 {
            return Err(FleaScopeError::DelaySamplesTooLarge {
                samples: delay_samples,
            });
        }

        log::debug!(
            "Reading with {} tick resolution with trigger {} and delay {}",
            number1,
            trigger_fields,
            delay_samples
        );

        let data = self.serial.exec(
            &format!("scope {} {} {}", number1, trigger_fields, delay_samples),
            None,
        )?;

        // Parse CSV data using Polars LazyFrames - you're absolutely right!
        // For cases where we might only need one column, LazyFrames provide significant benefits
        let df = CsvReadOptions::default()
            .with_has_header(false)
            .into_reader_with_file_handle(std::io::Cursor::new(data.as_bytes()))
            .finish().unwrap()
            .lazy()
            .select([
                col("column_1").alias("bnc").cast(DataType::Float64),
                col("column_2").alias("bitmap"),
            ])
            .with_row_index("row_index", Some(0))
            .with_columns([
                // Create time column using row index - more efficient than separate vector creation
                (col("row_index").cast(DataType::Float64) * lit(1.0 / (effective_msps * 1_000_000.0))).alias("time")
            ])
            .select([col("time"), col("bnc"), col("bitmap")]);

        Ok(df)
    }

    /// Extract bits from bitmap column
    pub fn extract_bits(df: &DataFrame) -> Result<DataFrame, FleaScopeError> {
        let bitmap_column = df.column("bitmap")?;
        let bitmap_strings = bitmap_column.str().map_err(|_| {
            FleaScopeError::DataParsing("Bitmap column is not string type".to_string())
        })?;

        // Parse hex strings and extract bits
        let mut bit_columns: Vec<Vec<bool>> = vec![Vec::new(); 10];

        for bitmap_opt in bitmap_strings.into_iter() {
            if let Some(bitmap_str) = bitmap_opt {
                let bitmap_str = bitmap_str.trim_start_matches("0x");
                if let Ok(bitmap_val) = u32::from_str_radix(bitmap_str, 16) {
                    for (bit, column) in bit_columns.iter_mut().enumerate().take(10) {
                        column.push((bitmap_val >> bit) & 1 == 1);
                    }
                } else {
                    // Default to false for unparseable values
                    for column in bit_columns.iter_mut().take(10) {
                        column.push(false);
                    }
                }
            } else {
                // Default to false for null values
                for column in bit_columns.iter_mut().take(10) {
                    column.push(false);
                }
            }
        }

        // Create new DataFrame with bit columns
        let mut df_data = vec![df.column("time")?.clone(), df.column("bnc")?.clone()];

        for (bit, values) in bit_columns.into_iter().enumerate() {
            df_data.push(Series::new(format!("bit_{}", bit).into(), values).into());
        }

        let result = DataFrame::new(df_data)?;
        Ok(result)
    }

    /// Send CTRL-C to unblock the device
    pub fn unblock(&mut self) -> Result<(), FleaScopeError> {
        self.serial.send_ctrl_c()?;
        Ok(())
    }

    /// Set the hostname
    pub fn set_hostname(&mut self, hostname: &str) -> Result<(), FleaScopeError> {
        self.serial.exec(&format!("hostname {}", hostname), None)?;
        self.hostname = hostname.to_string();
        Ok(())
    }

    /// Calibrate probe for 0V
    pub fn calibrate_zero(&mut self, probe: ProbeType) -> Result<f64, FleaScopeError> {
        // Try to preserve existing 3.3V calibration if available
        let raw_value_3v3 = {
            let probe_ref = self.get_probe(probe);
            if let (Some(_cal_zero), Some(_cal_3v3)) = probe_ref.calibration() {
                Some(probe_ref.voltage_to_raw(3.3).unwrap())
            } else {
                None
            }
        };

        // Read stable value for calibration
        let trigger_fields = DigitalTrigger::start_capturing_when()
            .is_matching()
            .into_trigger_fields();

        let data = self.raw_read(Duration::from_millis(20), &trigger_fields, None)?;
        let relevant_data = data.select([col("bnc"),]).collect()?;
        let bnc_series = relevant_data.column("bnc")?;
        let bnc_values: Vec<f64> = bnc_series.f64()?.into_no_null_iter().collect();
        
        if bnc_values.is_empty() {
            return Err(FleaScopeError::DataParsing("No data received".to_string()));
        }

        let min_val = bnc_values.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        let max_val = bnc_values.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));

        if max_val - min_val > 14.0 {
            return Err(FleaScopeError::SignalNotStable {
                min: min_val,
                max: max_val,
            });
        }

        let cal_zero = bnc_values.iter().sum::<f64>() / bnc_values.len() as f64;
        let probe_mut = self.get_probe_mut(probe);
        probe_mut.cal_zero = Some(cal_zero);

        if let Some(raw_3v3) = raw_value_3v3 {
            probe_mut.cal_3v3 = Some(raw_3v3 - cal_zero);
        }

        Ok(cal_zero)
    }

    /// Calibrate probe for 3.3V
    pub fn calibrate_3v3(&mut self, probe: ProbeType) -> Result<f64, FleaScopeError> {
        let cal_zero = {
            let probe_ref = self.get_probe(probe);
            probe_ref.cal_zero.ok_or(FleaScopeError::ZeroCalibrationRequired)?
        };
        
        // Read stable value for calibration
        let trigger_fields = DigitalTrigger::start_capturing_when()
            .is_matching()
            .into_trigger_fields();

        let data = self.raw_read(Duration::from_millis(20), &trigger_fields, None)?;
        let relevant_data = data.select([col("bnc"),]).collect()?;
        let bnc_series = relevant_data.column("bnc")?;
        let bnc_values: Vec<f64> = bnc_series.f64()?.into_no_null_iter().collect();
        
        if bnc_values.is_empty() {
            return Err(FleaScopeError::DataParsing("No data received".to_string()));
        }

        let min_val = bnc_values.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        let max_val = bnc_values.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));

        if max_val - min_val > 14.0 {
            return Err(FleaScopeError::SignalNotStable {
                min: min_val,
                max: max_val,
            });
        }

        let raw_3v3 = bnc_values.iter().sum::<f64>() / bnc_values.len() as f64;
        let cal_3v3 = raw_3v3 - cal_zero;
        let probe_mut = self.get_probe_mut(probe);
        probe_mut.cal_3v3 = Some(cal_3v3);
        Ok(cal_3v3)
    }

    /// Write probe calibration to flash
    pub fn write_calibration_to_flash(&mut self, probe: ProbeType) -> Result<(), FleaScopeError> {
        // Get calibration values first
        let (cal_zero, cal_3v3, multiplier) = {
            let probe_ref = self.get_probe(probe);
            (
                probe_ref.cal_zero.ok_or(FleaScopeError::CalibrationNotSet)?,
                probe_ref.cal_3v3.ok_or(FleaScopeError::CalibrationNotSet)?,
                probe_ref.multiplier,
            )
        };

        // Now write to flash
        let zero_value = (cal_zero - 2048.0 + 1000.0 + 0.5) as i32;
        let v3v3_value = (cal_3v3 * multiplier as f64 + 1000.0 + 0.5) as i32;

        self.serial.exec(
            &format!("cal_zero_x{} = {}", multiplier, zero_value),
            None,
        )?;
        self.serial.exec(
            &format!("cal_3v3_x{} = {}", multiplier, v3v3_value),
            None,
        )?;

        Ok(())
    }

    /// Helper method to get probe reference
    fn get_probe(&self, probe: ProbeType) -> &FleaProbe {
        match probe {
            ProbeType::X1 => &self.x1,
            ProbeType::X10 => &self.x10,
        }
    }

    /// Helper method to get mutable probe reference
    fn get_probe_mut(&mut self, probe: ProbeType) -> &mut FleaProbe {
        match probe {
            ProbeType::X1 => &mut self.x1,
            ProbeType::X10 => &mut self.x10,
        }
    }
}

impl Drop for FleaScope {
    fn drop(&mut self) {
        let _ = self.serial.exec("echo on", None);
        let _ = self.serial.exec("prompt on", None);
    }
}

#[derive(Debug)]
pub struct FleaProbe {
    multiplier: i32,
    cal_zero: Option<f64>, // value for 0V
    cal_3v3: Option<f64>,  // value-diff 0V - 3.3V
}

impl FleaProbe {
    /// Create a new probe with the given multiplier
    pub fn new(multiplier: i32) -> Self {
        Self {
            multiplier,
            cal_zero: None,
            cal_3v3: None,
        }
    }

    /// Read calibration values from flash
    pub fn read_calibration_from_flash(
        &mut self,
        serial: &mut FleaTerminal,
    ) -> Result<(), FleaScopeError> {
        let dim_result = serial.exec(
            &format!(
                "dim cal_zero_x{} as flash, cal_3v3_x{} as flash",
                self.multiplier, self.multiplier
            ),
            None,
        )?;

        let expected_response = format!(
            "var 'cal_zero_x{}' already declared at this scope\r\nvar 'cal_3v3_x{}' already declared at this scope",
            self.multiplier, self.multiplier
        );

        if dim_result == expected_response {
            log::debug!("Variables for calibration already declared. Reading values.");
        }

        let cal_zero_raw: i32 = serial
            .exec(&format!("print cal_zero_x{}", self.multiplier), None)?
            .trim()
            .parse()
            .map_err(|_| FleaScopeError::CalibrationNotSet)?;
        let cal_3v3_raw: i32 = serial
            .exec(&format!("print cal_3v3_x{}", self.multiplier), None)?
            .trim()
            .parse()
            .map_err(|_| FleaScopeError::CalibrationNotSet)?;

        self.cal_zero = Some((cal_zero_raw - 1000) as f64 + 2048.0);
        self.cal_3v3 = Some((cal_3v3_raw - 1000) as f64 / self.multiplier as f64);

        log::debug!(
            "Probe x{} calibration: cal_zero={:?}, cal_3v3={:?}",
            self.multiplier,
            self.cal_zero,
            self.cal_3v3
        );

        if let (Some(zero), Some(v3v3)) = (self.cal_zero, self.cal_3v3) {
            if (zero - v3v3).abs() < f64::EPSILON {
                return Err(FleaScopeError::CalibrationValuesEqual {
                    multiplier: self.multiplier,
                    value: zero,
                });
            }
        }

        Ok(())
    }

    /// Set calibration values manually
    pub fn set_calibration(&mut self, offset_0: f64, offset_3v3: f64) {
        self.cal_zero = Some(offset_0);
        self.cal_3v3 = Some(offset_3v3);
    }

    /// Write calibration values to flash
    pub fn write_calibration_to_flash(
        &self,
        serial: &mut FleaTerminal,
    ) -> Result<(), FleaScopeError> {
        let cal_zero = self.cal_zero.ok_or(FleaScopeError::CalibrationNotSet)?;
        let cal_3v3 = self.cal_3v3.ok_or(FleaScopeError::CalibrationNotSet)?;

        let zero_value = (cal_zero - 2048.0 + 1000.0 + 0.5) as i32;
        let v3v3_value = (cal_3v3 * self.multiplier as f64 + 1000.0 + 0.5) as i32;

        serial.exec(
            &format!("cal_zero_x{} = {}", self.multiplier, zero_value),
            None,
        )?;
        serial.exec(
            &format!("cal_3v3_x{} = {}", self.multiplier, v3v3_value),
            None,
        )?;

        Ok(())
    }

    /// Read a stable value for calibration purposes
    pub fn read_stable_value_for_calibration(
        &self,
        scope: &mut FleaScope,
    ) -> Result<f64, FleaScopeError> {
        let trigger_fields = DigitalTrigger::start_capturing_when()
            .is_matching()
            .into_trigger_fields();

        let data = scope.raw_read(Duration::from_millis(20), &trigger_fields, None)?;

        let relevant_data = data.select([col("bnc"),]).collect()?;
        let bnc_series = relevant_data.column("bnc")?;
        let bnc_values: Vec<f64> = bnc_series.f64()?.into_no_null_iter().collect();

        let min_val = bnc_values.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        let max_val = bnc_values.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));

        if max_val - min_val > 14.0 {
            return Err(FleaScopeError::SignalNotStable {
                min: min_val,
                max: max_val,
            });
        }

        let mean = bnc_values.iter().sum::<f64>() / bnc_values.len() as f64;
        Ok(mean)
    }

    /// Convert raw ADC value to voltage
    pub fn raw_to_voltage(&self, raw_value: Expr) -> Result<Expr, FleaScopeError> {
        let cal_zero = self.cal_zero.ok_or(FleaScopeError::CalibrationNotSet)?;
        let cal_3v3 = self.cal_3v3.ok_or(FleaScopeError::CalibrationNotSet)?;

        Ok((raw_value - cal_zero.into()) / cal_3v3.into() * 3.3.into())
    }

    /// Convert voltage to raw ADC value
    pub fn voltage_to_raw(&self, voltage: f64) -> Result<f64, FleaScopeError> {
        let cal_zero = self.cal_zero.ok_or(FleaScopeError::CalibrationNotSet)?;
        let cal_3v3 = self.cal_3v3.ok_or(FleaScopeError::CalibrationNotSet)?;

        Ok((voltage / 3.3 * cal_3v3) + cal_zero)
    }

    /// Calibrate for 0V
    pub fn calibrate_0(&mut self, scope: &mut FleaScope) -> Result<f64, FleaScopeError> {
        // Try to preserve existing 3.3V calibration if available
        let raw_value_3v3 = if let (Some(cal_zero), Some(cal_3v3)) = (self.cal_zero, self.cal_3v3) {
            Some(self.voltage_to_raw(3.3).unwrap_or(cal_3v3 + cal_zero))
        } else {
            None
        };

        self.cal_zero = Some(self.read_stable_value_for_calibration(scope)?);

        if let Some(raw_3v3) = raw_value_3v3 {
            self.cal_3v3 = Some(raw_3v3 - self.cal_zero.unwrap());
        }

        Ok(self.cal_zero.unwrap())
    }

    /// Calibrate for 3.3V
    pub fn calibrate_3v3(&mut self, scope: &mut FleaScope) -> Result<f64, FleaScopeError> {
        let cal_zero = self
            .cal_zero
            .ok_or(FleaScopeError::ZeroCalibrationRequired)?;

        let raw_3v3 = self.read_stable_value_for_calibration(scope)?;
        self.cal_3v3 = Some(raw_3v3 - cal_zero);

        Ok(self.cal_3v3.unwrap())
    }

    /// Get the multiplier value
    pub fn multiplier(&self) -> i32 {
        self.multiplier
    }

    /// Get calibration values
    pub fn calibration(&self) -> (Option<f64>, Option<f64>) {
        (self.cal_zero, self.cal_3v3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_waveform_as_str() {
        assert_eq!(Waveform::Sine.as_str(), "sine");
        assert_eq!(Waveform::Square.as_str(), "square");
        assert_eq!(Waveform::Triangle.as_str(), "triangle");
        assert_eq!(Waveform::Ekg.as_str(), "ekg");
    }

    #[test]
    fn test_duration_to_us() {
        assert_eq!(FleaScope::duration_to_us(Duration::from_millis(1)), 1000);
        assert_eq!(FleaScope::duration_to_us(Duration::from_micros(500)), 500);
    }

    #[test]
    fn test_number1_to_prescaler() {
        assert!(FleaScope::number1_to_prescaler(100).is_ok());
        assert!(FleaScope::number1_to_prescaler(0).is_err());
    }

    #[test]
    fn test_probe_calibration() {
        let mut probe = FleaProbe::new(1);
        assert_eq!(probe.calibration(), (None, None));

        probe.set_calibration(2048.0, 1000.0);
        assert_eq!(probe.calibration(), (Some(2048.0), Some(1000.0)));

        assert!(probe.voltage_to_raw(3.3).is_ok());
        assert!(probe.raw_to_voltage(3048.0.into()).is_ok());
    }
}
