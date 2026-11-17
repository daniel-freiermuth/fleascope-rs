use crate::flea_connector::{FleaConnector, FleaConnectorError};
use crate::serial_terminal::{
    BusyFleaTerminal, ConnectionLostError, FleaTerminalError, IdleFleaTerminal,
};
use crate::trigger_config::{DigitalTrigger, StringifiedTriggerConfig, TriggerConfig};
use polars::prelude::*;
use std::io::Read;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeType {
    X1,
    X10,
}

impl ProbeType {
    pub fn to_multiplier(&self) -> i32 {
        match self {
            Self::X1 => 1,
            Self::X10 => 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Copy)]
pub enum Waveform {
    Sine,
    Square,
    Triangle,
    Ekg,
}

impl Waveform {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sine => "sine",
            Self::Square => "square",
            Self::Triangle => "triangle",
            Self::Ekg => "ekg",
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

    #[error("Serial terminal error: {0}")]
    SerialTerminal(#[from] FleaTerminalError),

    #[error("Calibration error: {0}")]
    Calibration(#[from] CalibrationError),
}

#[derive(Debug, thiserror::Error)]
pub enum CalibrationError {
    #[error("No zero calibration available for this probe")]
    NoZeroCalibrarion,

    #[error("No calibrarion available for this probe")]
    NoCalibrationPresent,

    #[error("Signal to unstable")]
    UnstableSignal,

    #[error("Failure while processing calibration data")]
    CalibrationDataError(#[from] PolarsError),

    #[error("Serial terminal error: {0}")]
    SerialTerminal(#[from] FleaTerminalError),
}

pub struct ScopeReading {
    pub effective_msps: f64,
    pub data: Vec<u8>,
}

pub const RAW_COLUMN_NAME: &str = "bnc_raw";
pub const CALIBRATED_COLUMN_NAME: &str = "bnc_calibrated";
pub const BITMAP_COLUMN_NAME: &str = "bitmap";
pub const TIME_COLUMN_NAME: &str = "time";

impl ScopeReading {
    pub fn parse_csv(&self) -> Result<LazyFrame, PolarsError> {
        profiling::scope!("parse_csv");

        let df = CsvReadOptions::default()
            .with_has_header(false)
            // The firmware prints `%d,%x`: bitmaps are unprefixed hex, so an
            // all-digit capture would otherwise infer as an integer column.
            .with_dtype_overwrite(Some(Arc::new(vec![DataType::Float64, DataType::String])))
            .into_reader_with_file_handle(std::io::Cursor::new(&self.data))
            .finish()?
            .lazy()
            .select([
                col("column_1").alias(RAW_COLUMN_NAME),
                col("column_2").alias(BITMAP_COLUMN_NAME),
            ])
            .with_row_index("row_index", Some(0))
            .with_columns([
                // Create time column using row index - more efficient than separate vector creation
                (col("row_index").cast(DataType::Float64)
                    * lit(1.0 / (self.effective_msps * 1_000_000.0)))
                .alias(TIME_COLUMN_NAME),
            ])
            .select([
                col(TIME_COLUMN_NAME),
                col(RAW_COLUMN_NAME),
                col(BITMAP_COLUMN_NAME),
            ]);

        Ok(df)
    }

    /// Extract bits from bitmap column
    pub fn extract_bits(mut df: &mut DataFrame) -> Result<&DataFrame, PolarsError> {
        profiling::scope!("extract_bits");

        let bitmap_column = df.column(BITMAP_COLUMN_NAME)?;
        let bitmap_strings = bitmap_column.str()?;

        // Parse hex strings and extract bits
        let mut bit_columns: Vec<Vec<bool>> = vec![Vec::new(); 10];

        for bitmap_opt in bitmap_strings.iter() {
            if let Some(bitmap_str) = bitmap_opt
                && let Ok(bitmap_val) = u32::from_str_radix(bitmap_str.trim_start_matches("0x"), 16)
            {
                for (bit, column) in bit_columns.iter_mut().enumerate().take(10) {
                    column.push((bitmap_val >> bit) & 1 == 1);
                }
            } else {
                // Default to false for null or unparseable values
                for column in bit_columns.iter_mut().take(10) {
                    column.push(false);
                }
            }
        }

        for (bit, values) in bit_columns.into_iter().enumerate() {
            let column: Column = Series::new(format!("bit_{bit}").into(), values).into();
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
    ) -> Result<Result<(IdleFleaScope, ScopeReading), Self>, ConnectionLostError> {
        profiling::scope!("try_get_result");

        match self.serial.try_get_result()? {
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
        }
    }
    pub fn cancel(self) -> Result<IdleFleaScope, FleaTerminalError> {
        let idle_serial = self.serial.cancel()?;
        Ok(IdleFleaScope {
            serial: idle_serial,
            _ver: self._ver,
            hostname: self.hostname,
        })
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

    /// Connect to a `FleaScope` device
    pub fn connect(
        name: Option<&str>,
        port: Option<&str>,
        read_calibrations: bool,
    ) -> Result<(Self, FleaProbe, FleaProbe), FleaConnectorError> {
        let serial = FleaConnector::connect(name, port, true)?;
        let mut x1 = FleaProbe::new(ProbeType::X1);
        let mut x10 = FleaProbe::new(ProbeType::X10);

        let mut scope = Self::new(serial)?;
        if read_calibrations {
            x1.read_calibration_from_flash(&mut scope.serial)?;
            x10.read_calibration_from_flash(&mut scope.serial)?;
        }
        Ok((scope, x1, x10))
    }

    /// Create a new `FleaScope` from an existing terminal connection
    pub fn new(mut serial: IdleFleaTerminal) -> Result<Self, FleaTerminalError> {
        log::debug!("Turning off echo");
        serial.exec_sync("echo off", None)?;

        let ver = String::from_utf8_lossy(&serial.exec_sync("ver", None)?).into_owned();
        log::debug!("FleaScope version: {ver}");
        // TODO: check if version is compatible

        let hostname = String::from_utf8_lossy(&serial.exec_sync("hostname", None)?).into_owned();
        log::debug!("FleaScope hostname: {hostname}");
        // TODO: check if hostname is correct

        Ok(Self {
            serial,
            _ver: ver,
            hostname,
        })
    }

    /// Set the waveform generator
    pub fn set_waveform(&mut self, waveform: Waveform, hz: i32) -> Result<(), FleaTerminalError> {
        self.serial
            .exec_sync(&format!("wave {} {}", waveform.as_str(), hz), None)?;
        Ok(())
    }

    /// Convert number1 to prescaler value
    fn number1_to_prescaler(number1: u32) -> Result<u32, CaptureConfigError> {
        let ps = if number1 > 1000 { 16 } else { 1 };
        let t = ((Self::MCU_MHZ * f64::from(number1 * Self::INTERLEAVE)
            / f64::from(ps)
            / f64::from(Self::MSPS))
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
        Self::MCU_MHZ * f64::from(Self::INTERLEAVE) / f64::from(prescaler)
    }

    fn prepare_read_command(
        time_frame: Duration,
        trigger_fields: StringifiedTriggerConfig,
        delay: Option<Duration>,
    ) -> Result<(f64, String), CaptureConfigError> {
        profiling::scope!("prepare_read_command");

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
    ) -> Result<ReadingFleaScope, (Self, CaptureConfigError)> {
        profiling::scope!("read_async");

        match Self::prepare_read_command(time_frame, trigger_fields, delay) {
            Ok((effective_msps, command)) => match self.serial.exec_async(&command) {
                Ok(data) => Ok(ReadingFleaScope {
                    _ver: self._ver,
                    hostname: self.hostname,
                    serial: data,
                    effective_msps,
                }),
                Err((serial, e)) => Err((
                    Self {
                        serial,
                        _ver: self._ver,
                        hostname: self.hostname,
                    },
                    e.into(),
                )),
            },
            Err(e) => Err((self, e)),
        }
    }

    pub fn read_sync(
        &mut self,
        time_frame: Duration,
        trigger_fields: StringifiedTriggerConfig,
        delay: Option<Duration>,
    ) -> Result<ScopeReading, CaptureConfigError> {
        profiling::scope!("read_sync");

        let (effective_msps, command) =
            Self::prepare_read_command(time_frame, trigger_fields, delay)?;

        let data = self.serial.exec_sync(&command, None)?;
        Ok(ScopeReading {
            effective_msps,
            data,
        })
    }

    pub fn stream(self) -> Result<StreamingScope, (Self, FleaTerminalError)> {
        match self.serial.exec_async("stream") {
            Ok(serial) => Ok(StreamingScope {
                _ver: self._ver,
                hostname: self.hostname,
                serial,
            }),
            Err((serial, e)) => Err((
                Self {
                    serial,
                    _ver: self._ver,
                    hostname: self.hostname,
                },
                e,
            )),
        }
    }

    /// Set the hostname
    pub fn set_hostname(&mut self, hostname: &str) -> Result<(), FleaTerminalError> {
        self.serial
            .exec_sync(&format!("hostname {hostname}"), None)?;
        self.hostname = hostname.to_string();
        Ok(())
    }

    pub fn teardown(mut self) {
        let _ = self.serial.exec_sync("echo on", None);
        let _ = self.serial.exec_sync("prompt on", None);
    }
}

pub struct StreamingScope {
    _ver: String,
    hostname: String,
    serial: BusyFleaTerminal,
}

impl StreamingScope {
    pub fn stop(self) -> Result<IdleFleaScope, FleaTerminalError> {
        let serial = self.serial.cancel()?;
        Ok(IdleFleaScope {
            serial,
            _ver: self._ver,
            hostname: self.hostname,
        })
    }

    pub fn read(&mut self, n: usize) -> Result<Vec<u16>, std::io::Error> {
        profiling::scope!("StreamingScope::read");
        let mut buffer = vec![0u8; n * 2];
        self.serial.read_exact(&mut buffer)?;
        Ok(buffer
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect())
    }
}

#[derive(Debug, Clone)]
pub struct FleaProbe {
    multiplier: ProbeType,
    cal_zero: Option<f64>, // value for 0V
    cal_3v3: Option<f64>,  // value-diff 0V - 3.3V
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

    pub fn read_calibration_from_flash(
        &mut self,
        serial: &mut IdleFleaTerminal,
    ) -> Result<(), FleaTerminalError> {
        let dim_result = String::from_utf8_lossy(&serial.exec_sync(
            &format!(
                "dim cal_zero_x{} as flash, cal_3v3_x{} as flash",
                self.multiplier.to_multiplier(),
                self.multiplier.to_multiplier()
            ),
            None,
        )?)
        .into_owned();

        let expected_response = format!(
            "var 'cal_zero_x{}' already declared at this scope\r\nvar 'cal_3v3_x{}' already declared at this scope",
            self.multiplier.to_multiplier(),
            self.multiplier.to_multiplier()
        );

        if dim_result == expected_response {
            log::debug!("Variables for calibration already declared. Reading values.");
        }

        let cal_zero_command = format!("print cal_zero_x{}", self.multiplier.to_multiplier());
        let cal_zero_raw = parse_calibration_reply(
            &cal_zero_command,
            &serial.exec_sync(&cal_zero_command, None)?,
        )?;
        let cal_3v3_command = format!("print cal_3v3_x{}", self.multiplier.to_multiplier());
        let cal_3v3_raw =
            parse_calibration_reply(&cal_3v3_command, &serial.exec_sync(&cal_3v3_command, None)?)?;

        self.cal_zero = Some(f64::from(cal_zero_raw - 1000) + 2048.0);
        self.cal_3v3 =
            Some(f64::from(cal_3v3_raw - 1000) / f64::from(self.multiplier.to_multiplier()));

        log::debug!(
            "Probe x{} calibration: cal_zero={:?}, cal_3v3={:?}",
            self.multiplier.to_multiplier(),
            self.cal_zero,
            self.cal_3v3
        );
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
        scope: &mut IdleFleaScope,
    ) -> Result<(), CalibrationError> {
        let cal_zero = self
            .cal_zero
            .ok_or(CalibrationError::NoCalibrationPresent)?;
        let cal_3v3 = self.cal_3v3.ok_or(CalibrationError::NoCalibrationPresent)?;

        let zero_value = (cal_zero - 2048.0 + 1000.0 + 0.5) as i32;
        let v3v3_value =
            (cal_3v3.mul_add(f64::from(self.multiplier.to_multiplier()), 1000.0) + 0.5) as i32;

        scope.serial.exec_sync(
            &format!(
                "cal_zero_x{} = {}",
                self.multiplier.to_multiplier(),
                zero_value
            ),
            None,
        )?;
        scope.serial.exec_sync(
            &format!(
                "cal_3v3_x{} = {}",
                self.multiplier.to_multiplier(),
                v3v3_value
            ),
            None,
        )?;

        Ok(())
    }

    pub fn apply_calibration(&self, df: LazyFrame) -> Result<LazyFrame, CalibrationError> {
        profiling::scope!("apply_calibration");

        Ok(df.with_column(
            self.raw_to_voltage(col(RAW_COLUMN_NAME))?
                .alias(CALIBRATED_COLUMN_NAME),
        ))
    }

    /// Read a stable value for calibration purposes
    pub fn read_stable_value_for_calibration(
        &self,
        scope: &mut IdleFleaScope,
    ) -> Result<f64, CalibrationError> {
        let trigger_fields = DigitalTrigger::start_capturing_when()
            .is_matching()
            .into_trigger_fields();

        let bnc_values: Vec<f64> =
            match scope.read_sync(Duration::from_millis(20), trigger_fields, None) {
                Err(CaptureConfigError::SerialTerminal(e)) => return Err(e.into()),
                reading => reading.expect(
                    "This should not fail, as we are reading a stable value for calibration",
                ),
            }
            .parse_csv()?
            .select([col(RAW_COLUMN_NAME)])
            .collect()?
            .column(RAW_COLUMN_NAME)?
            .f64()?
            .into_no_null_iter()
            .collect();

        let min_val = bnc_values.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        let max_val = bnc_values.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));

        if max_val - min_val > 14.0 {
            return Err(CalibrationError::UnstableSignal);
        }

        let mean = bnc_values.iter().sum::<f64>() / bnc_values.len() as f64;
        Ok(mean)
    }

    /// Convert raw ADC value to voltage
    pub fn raw_to_voltage(&self, raw_value: Expr) -> Result<Expr, CalibrationError> {
        let cal_zero = self
            .cal_zero
            .ok_or(CalibrationError::NoCalibrationPresent)?;
        let cal_3v3 = self.cal_3v3.ok_or(CalibrationError::NoCalibrationPresent)?;

        Ok((raw_value - cal_zero.into()) / cal_3v3.into() * 3.3.into())
    }

    /// Convert voltage to raw ADC value
    pub fn voltage_to_raw(&self, voltage: f64) -> Result<f64, CalibrationError> {
        let cal_zero = self
            .cal_zero
            .ok_or(CalibrationError::NoCalibrationPresent)?;
        let cal_3v3 = self.cal_3v3.ok_or(CalibrationError::NoCalibrationPresent)?;

        Ok((voltage / 3.3).mul_add(cal_3v3, cal_zero))
    }

    /// Calibrate for 0V
    pub fn calibrate_0(&mut self, scope: &mut IdleFleaScope) -> Result<f64, CalibrationError> {
        // Try to preserve existing 3.3V calibration if available
        let raw_value_3v3 = self.voltage_to_raw(3.3).ok();

        let cal_zero = self.read_stable_value_for_calibration(scope)?;
        self.cal_zero = Some(cal_zero);

        if let Some(raw_3v3) = raw_value_3v3 {
            self.cal_3v3 = Some(raw_3v3 - cal_zero);
        }

        Ok(cal_zero)
    }

    /// Calibrate for 3.3V
    pub fn calibrate_3v3(&mut self, scope: &mut IdleFleaScope) -> Result<f64, CalibrationError> {
        let cal_zero = self.cal_zero.ok_or(CalibrationError::NoZeroCalibrarion)?;

        let raw_3v3 = self.read_stable_value_for_calibration(scope)?;
        let cal_3v3 = raw_3v3 - cal_zero;
        self.cal_3v3 = Some(cal_3v3);

        Ok(cal_3v3)
    }

    /// Get calibration values
    pub fn calibration(&self) -> (Option<f64>, Option<f64>) {
        (self.cal_zero, self.cal_3v3)
    }
}

/// Parse the integer the device prints for a calibration variable. Anything
/// else (a `StickOS` error message, line noise, non-UTF-8 bytes) is reported as
/// an unexpected response rather than trusted.
fn parse_calibration_reply(command: &str, reply: &[u8]) -> Result<i32, FleaTerminalError> {
    std::str::from_utf8(reply)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| FleaTerminalError::UnexpectedResponse {
            command: command.into(),
            response: String::from_utf8_lossy(reply).into(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AnalogTrigger;

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

    /// Four samples as the firmware prints them: `printf("%d,%x\n", adc, bits)`
    /// (fleascope-fw cpustick/scope-command.c), i.e. unprefixed hex bitmaps,
    /// with the CRLF line endings of the serial terminal.
    const CAPTURE: &[u8] = b"2048,0\r\n2100,3ff\r\n1996,205\r\n2500,0\r\n";

    fn reading_of(data: &[u8]) -> ScopeReading {
        ScopeReading {
            effective_msps: 2.0,
            data: data.to_vec(),
        }
    }

    fn reading() -> ScopeReading {
        reading_of(CAPTURE)
    }

    fn bitmap_strings(df: &DataFrame) -> Vec<Option<&str>> {
        df.column(BITMAP_COLUMN_NAME)
            .unwrap()
            .str()
            .unwrap()
            .iter()
            .collect()
    }

    fn f64_column(df: &DataFrame, name: &str) -> Vec<f64> {
        df.column(name)
            .unwrap()
            .f64()
            .unwrap()
            .into_no_null_iter()
            .collect()
    }

    fn bool_column(df: &DataFrame, name: &str) -> Vec<bool> {
        df.column(name)
            .unwrap()
            .bool()
            .unwrap()
            .iter()
            .collect::<Option<Vec<bool>>>()
            .unwrap()
    }

    fn assert_close(got: &[f64], want: &[f64]) {
        assert_eq!(got.len(), want.len(), "{got:?} != {want:?}");
        for (g, w) in got.iter().zip(want) {
            assert!((g - w).abs() < 1e-12, "{got:?} != {want:?}");
        }
    }

    #[test]
    fn parse_csv_yields_time_raw_and_bitmap_columns() {
        let df = reading().parse_csv().unwrap().collect().unwrap();

        let names: Vec<&str> = df.get_column_names().iter().map(|n| n.as_str()).collect();
        assert_eq!(
            names,
            [TIME_COLUMN_NAME, RAW_COLUMN_NAME, BITMAP_COLUMN_NAME]
        );
        // 2 MSPS → 0.5 µs per sample.
        assert_close(
            &f64_column(&df, TIME_COLUMN_NAME),
            &[0.0, 0.5e-6, 1.0e-6, 1.5e-6],
        );
        assert_eq!(
            f64_column(&df, RAW_COLUMN_NAME),
            [2048.0, 2100.0, 1996.0, 2500.0]
        );
        assert_eq!(
            bitmap_strings(&df),
            [Some("0"), Some("3ff"), Some("205"), Some("0")]
        );
    }

    #[test]
    fn parse_csv_keeps_all_digit_bitmaps_as_hex() {
        // No hex letters anywhere, so dtype inference alone would pick an integer.
        let mut df = reading_of(b"2048,0\r\n2049,10\r\n2047,205\r\n")
            .parse_csv()
            .unwrap()
            .collect()
            .unwrap();
        assert_eq!(bitmap_strings(&df), [Some("0"), Some("10"), Some("205")]);

        ScopeReading::extract_bits(&mut df).unwrap();
        // 0x10 = bit 4; 0x205 = bits 0, 2 and 9.
        assert_eq!(bool_column(&df, "bit_0"), [false, false, true]);
        assert_eq!(bool_column(&df, "bit_2"), [false, false, true]);
        assert_eq!(bool_column(&df, "bit_4"), [false, true, false]);
        assert_eq!(bool_column(&df, "bit_9"), [false, false, true]);
    }

    #[test]
    fn parse_csv_accepts_hex_letters_after_digit_only_rows() {
        // Hex letters first appear after polars' default 100-row inference window.
        let mut data = b"2048,0\r\n".repeat(150);
        data.extend_from_slice(b"2048,3ff\r\n");
        let mut df = reading_of(&data).parse_csv().unwrap().collect().unwrap();
        assert_eq!(df.height(), 151);
        assert_eq!(bitmap_strings(&df)[150], Some("3ff"));

        ScopeReading::extract_bits(&mut df).unwrap();
        let bit_9 = bool_column(&df, "bit_9");
        assert!(bit_9[150]);
        assert!(bit_9[..150].iter().all(|&b| !b));
    }

    #[test]
    fn extract_bits_splits_hex_bitmap_into_bit_columns() {
        let mut df = reading().parse_csv().unwrap().collect().unwrap();
        ScopeReading::extract_bits(&mut df).unwrap();

        // 0x205 = bits 0, 2 and 9.
        let expected: [[bool; 4]; 10] = [
            [false, true, true, false],
            [false, true, false, false],
            [false, true, true, false],
            [false, true, false, false],
            [false, true, false, false],
            [false, true, false, false],
            [false, true, false, false],
            [false, true, false, false],
            [false, true, false, false],
            [false, true, true, false],
        ];
        for (bit, want) in expected.iter().enumerate() {
            assert_eq!(bool_column(&df, &format!("bit_{bit}")), want, "bit_{bit}");
        }
        assert!(df.column("bit_10").is_err());
    }

    #[test]
    fn apply_calibration_maps_raw_to_volts() {
        let mut probe = FleaProbe::new(ProbeType::X1);
        // 2048 → 0 V, 2048 + 1000 → 3.3 V.
        probe.set_calibration(2048.0, 1000.0);

        let df = probe
            .apply_calibration(reading().parse_csv().unwrap())
            .unwrap()
            .collect()
            .unwrap();

        assert_close(
            &f64_column(&df, CALIBRATED_COLUMN_NAME),
            &[0.0, 0.1716, -0.1716, 1.4916],
        );
        assert_eq!(
            f64_column(&df, RAW_COLUMN_NAME),
            [2048.0, 2100.0, 1996.0, 2500.0]
        );
    }

    #[test]
    fn test_uncalibrated_probe_conversions_return_error() {
        let probe = FleaProbe::new(ProbeType::X1);
        assert!(matches!(
            probe.voltage_to_raw(1.0),
            Err(CalibrationError::NoCalibrationPresent)
        ));
        assert!(matches!(
            probe.raw_to_voltage(col(RAW_COLUMN_NAME)),
            Err(CalibrationError::NoCalibrationPresent)
        ));
        assert!(matches!(
            AnalogTrigger::start_capturing_when(1.0)
                .rising_edge()
                .into_trigger(&probe),
            Err(CaptureConfigError::Calibration(
                CalibrationError::NoCalibrationPresent
            ))
        ));
    }

    #[test]
    fn test_calibrated_probe_converts_voltage() {
        let mut probe = FleaProbe::new(ProbeType::X1);
        probe.set_calibration(2048.0, 1000.0);
        assert!(matches!(
            probe.voltage_to_raw(3.3),
            Ok(raw) if (raw - 3048.0).abs() < 1e-9
        ));
    }

    #[test]
    fn calibration_reply_parses_printed_integer() {
        assert_eq!(
            parse_calibration_reply("print cal_zero_x1", b"1023").unwrap(),
            1023
        );
        assert_eq!(
            parse_calibration_reply("print cal_3v3_x1", b" -7\r").unwrap(),
            -7
        );
    }

    #[test]
    fn calibration_reply_rejects_non_numeric_and_non_utf8() {
        for reply in [&b"error - undefined variable"[..], b"", b"10\xff23"] {
            let result = parse_calibration_reply("print cal_zero_x1", reply);
            assert!(
                matches!(
                    &result,
                    Err(FleaTerminalError::UnexpectedResponse { command, response })
                        if &**command == "print cal_zero_x1" && **response == *String::from_utf8_lossy(reply)
                ),
                "{reply:?} gave {result:?}"
            );
        }
    }
}
