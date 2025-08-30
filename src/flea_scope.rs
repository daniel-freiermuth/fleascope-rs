use crate::flea_connector::{FleaConnector, FleaConnectorError};
use crate::serial_terminal::{BusyFleaTerminal, ConnectionLostError, IdleFleaTerminal};
use crate::trigger_config::{DigitalTrigger, StringifiedTriggerConfig, TriggerConfig};
use polars::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeType {
    X1,
    X10,
}

impl ProbeType {
    pub fn to_multiplier(&self) -> i32 {
        match self {
            ProbeType::X1 => 1,
            ProbeType::X10 => 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Copy)]
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
pub enum CaptureConfigError {
    #[error("Time frame too large (max 3.49 seconds)")]
    TimeFrameTooLarge,

    #[error("Time frame too small (min 111 microseconds)")]
    TimeFrameTooSmall,

    #[error("Delay too large (max 1 second)")]
    DelayTooLarge,

    #[error("Voltage out of range")]
    VoltageOutOfRange,
}

#[derive(Debug, thiserror::Error)]
pub enum CalibrationError {
    #[error("No zero calibration available for this probe")]
    NoZeroCalibrarion,

    #[error("No calibrarion available for this probe")]
    NoCalibrationPresent,

    #[error("Signal to unstable")]
    UnstableSignal,

    #[error("Failure to get calibrartion data")]
    CalibrationDataError(#[from] PolarsError),
}

pub struct ScopeReading {
    pub effective_msps: f64,
    pub data: Vec<u8>,
}

const RAW_COLUMN_NAME: &str = "bnc_raw";
const CALIBRATED_COLUMN_NAME: &str = "bnc_calibrated";
const BITMAP_COLUMN_NAME: &str = "bitmap";
const TIME_COLUMN_NAME: &str = "time";

impl ScopeReading {
    pub fn parse_csv(&self) -> Result<LazyFrame, PolarsError> {
        #[cfg(feature = "puffin")]
        puffin::profile_function!();

        let df = CsvReadOptions::default()
            .with_has_header(false)
            .into_reader_with_file_handle(std::io::Cursor::new(&self.data))
            .finish()?
            .lazy()
            .select([
                col("column_1").alias(RAW_COLUMN_NAME).cast(DataType::Float64),
                col("column_2").alias(BITMAP_COLUMN_NAME),
            ])
            .with_row_index("row_index", Some(0))
            .with_columns([
                // Create time column using row index - more efficient than separate vector creation
                (col("row_index").cast(DataType::Float64)
                    * lit(1.0 / (self.effective_msps * 1_000_000.0)))
                .alias(TIME_COLUMN_NAME),
            ])
            .select([col(TIME_COLUMN_NAME), col(RAW_COLUMN_NAME), col(BITMAP_COLUMN_NAME)]);

        Ok(df)
    }

    /// Extract bits from bitmap column
    pub fn extract_bits(mut df: &mut DataFrame) -> Result<&DataFrame, PolarsError> {
        #[cfg(feature = "puffin")]
        puffin::profile_function!();

        let bitmap_column = df.column(BITMAP_COLUMN_NAME)?;
        let bitmap_strings = bitmap_column.str()?;

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


        for (bit, values) in bit_columns.into_iter().enumerate() {
            let column: Column = Series::new(format!("bit_{}", bit).into(), values).into();
            df = df.with_column(column)?;
        }

        Ok(df)
    }
}

pub struct ReadingFleaScope {
    _ver: String,
    hostname: String,
    serial: BusyFleaTerminal,
    effective_msps: f64,
}

impl ReadingFleaScope {
    pub fn try_get_result(
        mut self,
    ) -> Result<Result<(IdleFleaScope, ScopeReading), ReadingFleaScope>, ConnectionLostError> {
        #[cfg(feature = "puffin")]
        puffin::profile_function!();

        match self.serial.try_get_result() {
            Ok(r) => match r {
                Ok((data, idle_terminal)) => Ok(Ok((
                    IdleFleaScope {
                        serial: idle_terminal,
                        _ver: self._ver,
                        hostname: self.hostname,
                    },
                    ScopeReading {
                        effective_msps: self.effective_msps,
                        data,
                    },
                ))),
                Err(busy_terminal) => {
                    self.serial = busy_terminal;
                    Ok(Err(self))
                }
            },
            Err(e) => Err(e),
        }
    }
    pub fn cancel(self) -> IdleFleaScope{
        let idle_serial = self.serial.cancel();
        IdleFleaScope { serial: idle_serial,
            _ver: self._ver,
            hostname: self.hostname,
        }
    }
}

pub struct IdleFleaScope {
    serial: IdleFleaTerminal,
    _ver: String,
    hostname: String,
}

impl IdleFleaScope {
    // Constants
    const MSPS: u32 = 18; // Million samples per second. target sample rate
    const MCU_MHZ: f64 = 120.0; // MCU clock frequency in MHz, used for calculations
    const INTERLEAVE: u32 = 5; // number of ADCs interleaved
    const TOTAL_SAMPLES: u32 = 2000;

    /// Connect to a FleaScope device
    pub fn connect(
        name: Option<&str>,
        port: Option<&str>,
        read_calibrations: bool,
    ) -> Result<(Self, FleaProbe, FleaProbe), FleaConnectorError> {
        let serial = FleaConnector::connect(name, port, true)?;
        let mut x1 = FleaProbe::new(ProbeType::X1);
        let mut x10 = FleaProbe::new(ProbeType::X10);

        let mut scope = Self::new(serial);
        if read_calibrations {
            x1.read_calibration_from_flash(&mut scope.serial);
            x10.read_calibration_from_flash(&mut scope.serial);
        }
        Ok((scope, x1, x10))
    }

    /// Create a new FleaScope from an existing terminal connection
    pub fn new(mut serial: IdleFleaTerminal) -> Self {
        log::debug!("Turning off echo");
        serial.exec_sync("echo off", None);

        let ver = String::from_utf8(serial.exec_sync("ver", None)).expect("Failed to read version");
        log::debug!("FleaScope version: {}", ver);
        // TODO: check if version is compatible

        let hostname =
            String::from_utf8(serial.exec_sync("hostname", None)).expect("Failed to read hostname");
        log::debug!("FleaScope hostname: {}", hostname);
        // TODO: check if hostname is correct

        Self {
            serial,
            _ver: ver,
            hostname,
        }
    }

    /// Set the waveform generator
    pub fn set_waveform(&mut self, waveform: Waveform, hz: i32) {
        self.serial
            .exec_sync(&format!("wave {} {}", waveform.as_str(), hz), None);
    }

    /// Convert number1 to prescaler value
    fn number1_to_prescaler(number1: u32) -> Result<u32, CaptureConfigError> {
        let ps = if number1 > 1000 { 16 } else { 1 };
        let t =
            ((Self::MCU_MHZ * (number1 * Self::INTERLEAVE) as f64 / ps as f64 / Self::MSPS as f64)
                + 0.5) as u32;

        if t == 0 {
            return Err(CaptureConfigError::TimeFrameTooSmall);
        }
        if t > 65535 {
            return Err(CaptureConfigError::TimeFrameTooLarge);
        }

        Ok(ps * t)
    }

    /// Convert prescaler to effective MSPS
    fn prescaler_to_effective_msps(prescaler: u32) -> f64 {
        Self::MCU_MHZ * Self::INTERLEAVE as f64 / prescaler as f64
    }

    fn prepare_read_command(
        time_frame: Duration,
        trigger_fields: StringifiedTriggerConfig,
        delay: Option<Duration>,
    ) -> Result<(f64, String), CaptureConfigError> {
        #[cfg(feature = "puffin")]
        puffin::profile_function!();

        let delay = delay.unwrap_or(Duration::from_millis(0));

        // Validate time frame
        if time_frame.as_secs_f64() > 3.49 {
            return Err(CaptureConfigError::TimeFrameTooLarge);
        }
        if time_frame.as_secs() == 0 && time_frame.as_micros() < 111 {
            return Err(CaptureConfigError::TimeFrameTooSmall);
        }

        // Validate delay
        if delay.as_secs_f64() > 1.0 {
            return Err(CaptureConfigError::DelayTooLarge);
        }

        let number1 = Self::MSPS * (time_frame.as_micros() as u32) / Self::TOTAL_SAMPLES;
        if number1 == 0 {
            return Err(CaptureConfigError::TimeFrameTooSmall);
        }

        let prescaler = Self::number1_to_prescaler(number1)?;
        let effective_msps = Self::prescaler_to_effective_msps(prescaler);

        let delay_samples = (delay.as_micros() as f64 * effective_msps) as u32;
        if delay_samples > 1_000_000 {
            return Err(CaptureConfigError::DelayTooLarge);
        }
        Ok((
            effective_msps,
            format!(
                "scope {} {} {}",
                number1,
                trigger_fields.into_string(),
                delay_samples
            ),
        ))
    }

    /// Raw data read from the oscilloscope
    pub fn read_async(
        self,
        time_frame: Duration,
        trigger_fields: StringifiedTriggerConfig,
        delay: Option<Duration>,
    ) -> Result<ReadingFleaScope, (IdleFleaScope, CaptureConfigError)> {
        #[cfg(feature = "puffin")]
        puffin::profile_function!();

        match Self::prepare_read_command(time_frame, trigger_fields, delay) {
            Ok((effective_msps, command)) => {
                let data = self.serial.exec_async(&command);
                Ok(ReadingFleaScope {
                    _ver: self._ver,
                    hostname: self.hostname,
                    serial: data,
                    effective_msps,
                })
            }
            Err(e) => Err((self, e)),
        }
    }

    pub fn read_sync(
        &mut self,
        time_frame: Duration,
        trigger_fields: StringifiedTriggerConfig,
        delay: Option<Duration>,
    ) -> Result<ScopeReading, CaptureConfigError> {
        #[cfg(feature = "puffin")]
        puffin::profile_function!();

        let (effective_msps, command) =
            Self::prepare_read_command(time_frame, trigger_fields, delay)?;

        let data = self.serial.exec_sync(&command, None);
        Ok(ScopeReading {
            effective_msps,
            data,
        })
    }

    /// Set the hostname
    pub fn set_hostname(&mut self, hostname: &str) {
        self.serial
            .exec_sync(&format!("hostname {}", hostname), None);
        self.hostname = hostname.to_string();
    }

    pub fn teardown(mut self) {
        let _ = self.serial.exec_sync("echo on", None);
        let _ = self.serial.exec_sync("prompt on", None);
    }
}

#[derive(Debug)]
pub struct FleaProbe {
    multiplier: ProbeType,
    cal_zero: Option<f64>, // value for 0V
    cal_3v3: Option<f64>,  // value-diff 0V - 3.3V
}

impl Clone for FleaProbe {
    fn clone(&self) -> Self {
        Self {
            multiplier: self.multiplier,
            cal_zero: self.cal_zero,
            cal_3v3: self.cal_3v3,
        }
    }
}

impl FleaProbe {
    /// Create a new probe with the given multiplier
    pub fn new(multiplier: ProbeType) -> Self {
        Self {
            multiplier,
            cal_zero: None,
            cal_3v3: None,
        }
    }

    pub fn read_calibration_from_flash(&mut self, serial: &mut IdleFleaTerminal) {
        let dim_result = String::from_utf8(serial.exec_sync(
            &format!(
                "dim cal_zero_x{} as flash, cal_3v3_x{} as flash",
                self.multiplier.to_multiplier(),
                self.multiplier.to_multiplier()
            ),
            None,
        ))
        .expect("Failed to read calibration from flash");

        let expected_response = format!(
            "var 'cal_zero_x{}' already declared at this scope\r\nvar 'cal_3v3_x{}' already declared at this scope",
            self.multiplier.to_multiplier(), self.multiplier.to_multiplier()
        );

        if dim_result == expected_response {
            log::debug!("Variables for calibration already declared. Reading values.");
        }

        let cal_zero_raw: i32 = String::from_utf8(serial.exec_sync(
            &format!("print cal_zero_x{}", self.multiplier.to_multiplier()),
            None,
        ))
        .expect("Failed to read cal_zero_x value")
        .trim()
        .parse()
        .expect("Failed to parse cal_zero_x value");
        let cal_3v3_raw: i32 = String::from_utf8(serial.exec_sync(
            &format!("print cal_3v3_x{}", self.multiplier.to_multiplier()),
            None,
        ))
        .expect("Failed to read cal_3v3_x value")
        .trim()
        .parse()
        .expect("Failed to parse cal_3v3_x value");

        self.cal_zero = Some((cal_zero_raw - 1000) as f64 + 2048.0);
        self.cal_3v3 = Some((cal_3v3_raw - 1000) as f64 / self.multiplier.to_multiplier() as f64);

        log::debug!(
            "Probe x{} calibration: cal_zero={:?}, cal_3v3={:?}",
            self.multiplier.to_multiplier(),
            self.cal_zero,
            self.cal_3v3
        );
    }

    /// Set calibration values manually
    pub fn set_calibration(&mut self, offset_0: f64, offset_3v3: f64) {
        self.cal_zero = Some(offset_0);
        self.cal_3v3 = Some(offset_3v3);
    }

    /// Write calibration values to flash
    pub fn write_calibration_to_flash(
        &self,
        scope: &mut IdleFleaScope,
    ) -> Result<(), CalibrationError> {
        let cal_zero = self
            .cal_zero
            .ok_or(CalibrationError::NoCalibrationPresent)?;
        let cal_3v3 = self.cal_3v3.ok_or(CalibrationError::NoCalibrationPresent)?;

        let zero_value = (cal_zero - 2048.0 + 1000.0 + 0.5) as i32;
        let v3v3_value = (cal_3v3 * self.multiplier.to_multiplier() as f64 + 1000.0 + 0.5) as i32;

        scope.serial.exec_sync(
            &format!(
                "cal_zero_x{} = {}",
                self.multiplier.to_multiplier(),
                zero_value
            ),
            None,
        );
        scope.serial.exec_sync(
            &format!(
                "cal_3v3_x{} = {}",
                self.multiplier.to_multiplier(),
                v3v3_value
            ),
            None,
        );

        Ok(())
    }

    pub fn apply_calibration(&self, df: LazyFrame) -> LazyFrame {
        #[cfg(feature = "puffin")]
        puffin::profile_function!();

        df.with_column(self.raw_to_voltage(col(RAW_COLUMN_NAME)).alias(CALIBRATED_COLUMN_NAME))
    }

    /// Read a stable value for calibration purposes
    pub fn read_stable_value_for_calibration(
        &self,
        scope: &mut IdleFleaScope,
    ) -> Result<f64, CalibrationError> {
        let trigger_fields = DigitalTrigger::start_capturing_when()
            .is_matching()
            .into_trigger_fields();

        let reading = scope
            .read_sync(Duration::from_millis(20), trigger_fields, None)
            .expect("This should not fail, as we are reading a stable value for calibration");
        let df = reading.parse_csv()?;

        let relevant_data = df.select([col(RAW_COLUMN_NAME)]).collect()?;
        let bnc_series = relevant_data.column(RAW_COLUMN_NAME)?;
        let bnc_values: Vec<f64> = bnc_series.f64()?.into_no_null_iter().collect();

        let min_val = bnc_values.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        let max_val = bnc_values.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));

        if max_val - min_val > 14.0 {
            return Err(CalibrationError::UnstableSignal);
        }

        let mean = bnc_values.iter().sum::<f64>() / bnc_values.len() as f64;
        Ok(mean)
    }

    /// Convert raw ADC value to voltage
    pub fn raw_to_voltage(&self, raw_value: Expr) -> Expr {
        let cal_zero = self.cal_zero.expect("Calibration for 0V is not set");
        let cal_3v3 = self.cal_3v3.expect("Calibration for 3.3V is not set");

        (raw_value - cal_zero.into()) / cal_3v3.into() * 3.3.into()
    }

    /// Convert voltage to raw ADC value
    pub fn voltage_to_raw(&self, voltage: f64) -> f64 {
        let cal_zero = self.cal_zero.expect("Calibration for 0V is not set");
        let cal_3v3 = self.cal_3v3.expect("Calibration for 3.3V is not set");

        (voltage / 3.3 * cal_3v3) + cal_zero
    }

    /// Calibrate for 0V
    pub fn calibrate_0(&mut self, scope: &mut IdleFleaScope) -> Result<f64, CalibrationError> {
        // Try to preserve existing 3.3V calibration if available
        let raw_value_3v3 = if let (Some(_), Some(_)) = (self.cal_zero, self.cal_3v3) {
            Some(self.voltage_to_raw(3.3))
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
    pub fn calibrate_3v3(&mut self, scope: &mut IdleFleaScope) -> Result<f64, CalibrationError> {
        let cal_zero = self.cal_zero.ok_or(CalibrationError::NoZeroCalibrarion)?;

        let raw_3v3 = self.read_stable_value_for_calibration(scope)?;
        self.cal_3v3 = Some(raw_3v3 - cal_zero);

        Ok(self.cal_3v3.unwrap())
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
    fn test_number1_to_prescaler() {
        assert!(IdleFleaScope::number1_to_prescaler(100).is_ok());
        assert!(IdleFleaScope::number1_to_prescaler(0).is_err());
    }
}
