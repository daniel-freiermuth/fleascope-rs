use crate::flea_connector::{FleaConnector, FleaConnectorError};
use crate::serial_terminal::{
    BusyFleaTerminal, ConnectionLostError, FleaTerminalError, IdleFleaTerminal, TerminalStatus,
};
use crate::trigger_config::{DigitalTrigger, StringifiedTriggerConfig, TriggerConfig};
use std::io::Read;
use std::time::Duration;

/// How long a command other than a capture may take to return the prompt.
/// These answer right away; only a capture waits for its trigger.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

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

    #[error("Malformed capture: {0}")]
    MalformedCapture(#[from] ParseError),
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
    CalibrationDataError(#[from] ParseError),

    #[error("Serial terminal error: {0}")]
    SerialTerminal(#[from] FleaTerminalError),
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("Capture contained no samples")]
    Empty,

    #[error("Malformed capture line {line}: {content:?}")]
    MalformedLine {
        /// 0-based line index within the capture response.
        line: usize,
        content: String,
    },
}

/// One parsed capture: uniformly sampled analog ADC counts plus the digital bitmap.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeReading {
    /// Effective sample rate in million samples per second.
    msps: f64,
    /// Uncalibrated ADC counts of the BNC input; convert with [`FleaProbe::voltages`].
    raw_bnc: Vec<u16>,
    /// Digital inputs, bit `n` = channel `n`. Same length as `raw_bnc`.
    bitmap: Vec<u16>,
}

impl ScopeReading {
    pub fn len(&self) -> usize {
        self.raw_bnc.len()
    }

    pub fn is_empty(&self) -> bool {
        self.raw_bnc.is_empty()
    }

    /// Uncalibrated ADC counts of the BNC input, one per sample.
    pub fn raw_bnc(&self) -> &[u16] {
        &self.raw_bnc
    }

    /// Digital inputs, one bitmap per sample; bit `n` = channel `n`.
    pub fn bitmap(&self) -> &[u16] {
        &self.bitmap
    }

    /// Time of sample `index` in seconds from the start of the capture.
    pub fn time(&self, index: usize) -> f64 {
        index as f64 / (self.msps * 1_000_000.0)
    }

    /// Time of every sample in seconds from the start of the capture.
    pub fn times(&self) -> impl ExactSizeIterator<Item = f64> + '_ {
        (0..self.len()).map(|i| self.time(i))
    }

    /// State of digital `channel` for every sample. Channels beyond the
    /// bitmap width (16 and up) read as low.
    pub fn bit(&self, channel: u8) -> impl ExactSizeIterator<Item = bool> + '_ {
        self.bitmap.iter().map(move |b| {
            b.checked_shr(u32::from(channel))
                .is_some_and(|v| v & 1 == 1)
        })
    }

    /// Parse the device's `<adc>,<hex bitmap>` lines.
    fn from_wire_bytes(msps: f64, bytes: &[u8]) -> Result<Self, ParseError> {
        profiling::scope!("ScopeReading::from_wire_bytes");

        let mut raw = Vec::with_capacity(IdleFleaScope::TOTAL_SAMPLES as usize);
        let mut bitmap = Vec::with_capacity(IdleFleaScope::TOTAL_SAMPLES as usize);
        for (line, content) in bytes.split(|&b| b == b'\n').enumerate() {
            let content = content.trim_ascii();
            if content.is_empty() {
                continue;
            }
            let (adc, bits) =
                Self::parse_line(content).ok_or_else(|| ParseError::MalformedLine {
                    line,
                    content: String::from_utf8_lossy(content).into_owned(),
                })?;
            raw.push(adc);
            bitmap.push(bits);
        }
        if raw.is_empty() {
            return Err(ParseError::Empty);
        }
        Ok(Self {
            msps,
            raw_bnc: raw,
            bitmap,
        })
    }

    fn parse_line(line: &[u8]) -> Option<(u16, u16)> {
        let line = std::str::from_utf8(line).ok()?;
        let (adc, bits) = line.split_once(',')?;
        let bits = bits.trim();
        let bits = bits.strip_prefix("0x").unwrap_or(bits);
        Some((
            adc.trim().parse().ok()?,
            u16::from_str_radix(bits, 16).ok()?,
        ))
    }
}

/// Outcome of polling a [`ReadingFleaScope`].
#[must_use]
pub enum ReadStatus {
    /// Still capturing; poll again later.
    Pending(ReadingFleaScope),
    /// Capture finished; the scope is idle again whether or not the data parsed.
    Done(IdleFleaScope, Result<ScopeReading, ParseError>),
}

pub struct ReadingFleaScope {
    _ver: String,
    hostname: String,
    serial: BusyFleaTerminal,
    effective_msps: f64,
}

impl ReadingFleaScope {
    pub fn try_get_result(mut self) -> Result<ReadStatus, ConnectionLostError> {
        profiling::scope!("try_get_result");

        match self.serial.try_get_result()? {
            TerminalStatus::Done(data, idle_terminal) => Ok(ReadStatus::Done(
                IdleFleaScope {
                    serial: idle_terminal,
                    _ver: self._ver,
                    hostname: self.hostname,
                },
                ScopeReading::from_wire_bytes(self.effective_msps, &data),
            )),
            TerminalStatus::Pending(busy_terminal) => {
                self.serial = busy_terminal;
                Ok(ReadStatus::Pending(self))
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
        let serial = FleaConnector::connect(name, port)?;
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
        serial.exec_sync("echo off", Some(COMMAND_TIMEOUT))?;

        let ver =
            String::from_utf8_lossy(&serial.exec_sync("ver", Some(COMMAND_TIMEOUT))?).into_owned();
        log::debug!("FleaScope version: {ver}");
        // TODO: check if version is compatible

        let hostname =
            String::from_utf8_lossy(&serial.exec_sync("hostname", Some(COMMAND_TIMEOUT))?)
                .into_owned();
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
        self.serial.exec_sync(
            &format!("wave {} {}", waveform.as_str(), hz),
            Some(COMMAND_TIMEOUT),
        )?;
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

        // No timeout: the capture waits for its trigger, which may take arbitrarily long.
        let data = self.serial.exec_sync(&command, None)?;
        Ok(ScopeReading::from_wire_bytes(effective_msps, &data)?)
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
            .exec_sync(&format!("hostname {hostname}"), Some(COMMAND_TIMEOUT))?;
        self.hostname = hostname.to_string();
        Ok(())
    }

    pub fn teardown(mut self) {
        let _ = self.serial.exec_sync("echo on", Some(COMMAND_TIMEOUT));
        let _ = self.serial.exec_sync("prompt on", Some(COMMAND_TIMEOUT));
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
            Some(COMMAND_TIMEOUT),
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
            &serial.exec_sync(&cal_zero_command, Some(COMMAND_TIMEOUT))?,
        )?;
        let cal_3v3_command = format!("print cal_3v3_x{}", self.multiplier.to_multiplier());
        let cal_3v3_raw = parse_calibration_reply(
            &cal_3v3_command,
            &serial.exec_sync(&cal_3v3_command, Some(COMMAND_TIMEOUT))?,
        )?;

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
            Some(COMMAND_TIMEOUT),
        )?;
        scope.serial.exec_sync(
            &format!(
                "cal_3v3_x{} = {}",
                self.multiplier.to_multiplier(),
                v3v3_value
            ),
            Some(COMMAND_TIMEOUT),
        )?;

        Ok(())
    }

    /// Calibrated BNC voltages for `raw` ADC counts, e.g. [`ScopeReading::raw_bnc`]
    /// or a batch from [`StreamingScope::read`].
    pub fn voltages<'a>(
        &self,
        raw: &'a [u16],
    ) -> Result<impl ExactSizeIterator<Item = f64> + 'a, CalibrationError> {
        let (cal_zero, cal_3v3) = self.require_calibration()?;
        Ok(raw
            .iter()
            .map(move |&r| Self::to_voltage(f64::from(r), cal_zero, cal_3v3)))
    }

    /// Read a stable value for calibration purposes
    pub fn read_stable_value_for_calibration(
        &self,
        scope: &mut IdleFleaScope,
    ) -> Result<f64, CalibrationError> {
        let trigger_fields = DigitalTrigger::start_capturing_when()
            .is_matching()
            .into_trigger_fields();

        let capture = match scope.read_sync(Duration::from_millis(20), trigger_fields, None) {
            Err(CaptureConfigError::SerialTerminal(e)) => return Err(e.into()),
            Err(CaptureConfigError::MalformedCapture(e)) => return Err(e.into()),
            reading => reading
                .expect("This should not fail, as we are reading a stable value for calibration"),
        };

        let min_val = capture
            .raw_bnc
            .iter()
            .min()
            .map_or(f64::INFINITY, |&v| f64::from(v));
        let max_val = capture
            .raw_bnc
            .iter()
            .max()
            .map_or(f64::NEG_INFINITY, |&v| f64::from(v));

        if max_val - min_val > 14.0 {
            return Err(CalibrationError::UnstableSignal);
        }

        let sum: f64 = capture.raw_bnc.iter().map(|&v| f64::from(v)).sum();
        Ok(sum / capture.len() as f64)
    }

    /// Convert raw ADC value to voltage
    pub fn raw_to_voltage(&self, raw_value: f64) -> Result<f64, CalibrationError> {
        let (cal_zero, cal_3v3) = self.require_calibration()?;
        Ok(Self::to_voltage(raw_value, cal_zero, cal_3v3))
    }

    fn to_voltage(raw_value: f64, cal_zero: f64, cal_3v3: f64) -> f64 {
        (raw_value - cal_zero) / cal_3v3 * 3.3
    }

    fn require_calibration(&self) -> Result<(f64, f64), CalibrationError> {
        Ok((
            self.cal_zero
                .ok_or(CalibrationError::NoCalibrationPresent)?,
            self.cal_3v3.ok_or(CalibrationError::NoCalibrationPresent)?,
        ))
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
        ScopeReading::from_wire_bytes(2.0, data).unwrap()
    }

    fn reading() -> ScopeReading {
        reading_of(CAPTURE)
    }

    fn assert_close(got: &[f64], want: &[f64]) {
        assert_eq!(got.len(), want.len(), "{got:?} != {want:?}");
        for (g, w) in got.iter().zip(want) {
            assert!((g - w).abs() < 1e-12, "{got:?} != {want:?}");
        }
    }

    #[test]
    fn parse_yields_time_raw_and_bitmap() {
        let capture = reading();

        // 2 MSPS → 0.5 µs per sample.
        assert_close(
            &capture.times().collect::<Vec<_>>(),
            &[0.0, 0.5e-6, 1.0e-6, 1.5e-6],
        );
        assert_eq!(capture.raw_bnc(), [2048, 2100, 1996, 2500]);
        assert_eq!(capture.bitmap(), [0x0, 0x3ff, 0x205, 0x0]);
    }

    #[test]
    fn parse_reads_all_digit_bitmaps_as_hex() {
        let capture = reading_of(b"2048,0\r\n2049,10\r\n2047,205\r\n");

        // 0x10 = bit 4; 0x205 = bits 0, 2 and 9.
        assert_eq!(capture.bit(0).collect::<Vec<_>>(), [false, false, true]);
        assert_eq!(capture.bit(2).collect::<Vec<_>>(), [false, false, true]);
        assert_eq!(capture.bit(4).collect::<Vec<_>>(), [false, true, false]);
        assert_eq!(capture.bit(9).collect::<Vec<_>>(), [false, false, true]);
    }

    #[test]
    fn bit_splits_hex_bitmap_into_channels() {
        let capture = reading();

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
        for (bit, want) in (0u8..).zip(&expected) {
            assert_eq!(&capture.bit(bit).collect::<Vec<_>>(), want, "bit_{bit}");
        }
    }

    #[test]
    fn voltages_maps_raw_to_volts() {
        let mut probe = FleaProbe::new(ProbeType::X1);
        // 2048 → 0 V, 2048 + 1000 → 3.3 V.
        probe.set_calibration(2048.0, 1000.0);

        let volts: Vec<f64> = probe.voltages(reading().raw_bnc()).unwrap().collect();

        assert_close(&volts, &[0.0, 0.1716, -0.1716, 1.4916]);
    }

    #[test]
    fn test_uncalibrated_probe_conversions_return_error() {
        let probe = FleaProbe::new(ProbeType::X1);
        assert!(matches!(
            probe.voltage_to_raw(1.0),
            Err(CalibrationError::NoCalibrationPresent)
        ));
        assert!(matches!(
            probe.raw_to_voltage(2048.0),
            Err(CalibrationError::NoCalibrationPresent)
        ));
        assert!(matches!(
            probe.voltages(&[2048]).map(Iterator::count),
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

    #[test]
    fn test_parse_capture() {
        // Firmware prints "%d,%x"; older builds prefixed the bitmap with "0x".
        let capture = reading_of(b"2048,1ff\r\n100,0x3\n\n4095,0\r\n");
        assert_eq!(capture.raw_bnc(), [2048, 100, 4095]);
        assert_eq!(capture.bitmap(), [0x1ff, 0x3, 0]);
        assert_eq!(capture.times().collect::<Vec<_>>(), [0.0, 0.5e-6, 1.0e-6]);
        assert_eq!(capture.bit(1).collect::<Vec<_>>(), [true, true, false]);
        assert_eq!(capture.bit(8).collect::<Vec<_>>(), [true, false, false]);
    }

    #[test]
    fn test_parse_capture_rejects_malformed_line() {
        let err = ScopeReading::from_wire_bytes(1.0, b"1,0\nnot a sample\n").unwrap_err();
        assert!(
            matches!(&err, ParseError::MalformedLine { line: 1, content } if content == "not a sample"),
            "{err:?}"
        );
    }

    #[test]
    fn test_parse_capture_rejects_empty_response() {
        for data in [&b""[..], b"\r\n\r\n"] {
            assert!(matches!(
                ScopeReading::from_wire_bytes(1.0, data),
                Err(ParseError::Empty)
            ));
        }
    }

    #[test]
    fn bit_beyond_bitmap_width_reads_low() {
        let capture = reading_of(b"1,ffff\n");
        assert_eq!(capture.bit(15).collect::<Vec<_>>(), [true]);
        assert_eq!(capture.bit(16).collect::<Vec<_>>(), [false]);
        assert_eq!(capture.bit(255).collect::<Vec<_>>(), [false]);
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "macOS ptys reject the IOSSIOSPEED ioctl serialport uses to set the baud rate on open"
    )]
    fn new_times_out_when_device_stops_answering() {
        use crate::serial_terminal::StatelessFleaTerminal;
        use std::io::{ErrorKind, Write};

        // The master end plays a device that answers the `prompt on` of the
        // terminal init, then stays silent but connected.
        let (mut master, slave) = serialport::TTYPort::pair().unwrap();
        let slave_path = serialport::SerialPort::name(&slave).unwrap();
        drop(slave);
        let stateless = StatelessFleaTerminal::new(&slave_path).unwrap();
        let device = std::thread::spawn(move || {
            let mut received = Vec::new();
            let mut buf = [0u8; 64];
            while !received.ends_with(b"prompt on\n") {
                match master.read(&mut buf) {
                    Ok(n) => received.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == ErrorKind::TimedOut => {}
                    Err(e) => panic!("device read failed: {e}"),
                }
            }
            master.write_all(b"> ").unwrap();
            master
        });
        let idle = IdleFleaTerminal::try_from(stateless)
            .map_err(|(_, e)| e)
            .unwrap();
        let _master = device.join().unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(IdleFleaScope::new(idle).err());
        });
        // `echo off` must give up after COMMAND_TIMEOUT; allow generous slack.
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Some(FleaTerminalError::Timeout { timeout })) => {
                assert_eq!(timeout, COMMAND_TIMEOUT);
            }
            Ok(other) => panic!("expected Timeout error, got {other:?}"),
            Err(_) => panic!("IdleFleaScope::new() did not return within 15s on a silent device"),
        }
    }
}
