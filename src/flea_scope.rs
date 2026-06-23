use crate::flea_connector::{FleaConnector, FleaConnectorError};
use crate::serial_terminal::{
    BusyFleaTerminal, ConnectionLostError, FleaTerminalError, IdleFleaTerminal, TerminalStatus,
};
use crate::trigger_config::{DigitalTrigger, StringifiedTriggerConfig, TriggerConfig};
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

    #[error("Invalid digital channel mask {0:#x} (must be non-zero and within 0x1ff)")]
    InvalidChannelMask(u16),

    #[error("Invalid samples per packet {requested} (must be 1..={max} for this channel mask)")]
    InvalidSamplesPerPacket { requested: u32, max: u32 },

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
    /// Bits in one digital-stream packet: the firmware's 63-byte USB FS buffer.
    const DIGITAL_PACKET_BITS: u32 = 63 * 8;

    /// Default `sample_length` for the `stream` command.
    ///
    /// This is the raw timer-period register value (`PR3 + 1`) passed to the firmware.
    /// It corresponds to the historical hardcoded value of `number1 = 70` at a 120 MHz
    /// bus clock, yielding an ADC trigger frequency of roughly 51 kHz.
    pub const STREAM_SAMPLE_LENGTH: u32 = 2333;

    /// Default averaging factor for the `stream` command (1 = no averaging).
    pub const STREAM_AVERAGING: u32 = 1;

    /// Default bit-width per sample for the `stream` command.
    ///
    /// 16 means each sample occupies exactly two bytes in little-endian order,
    /// matching the `u16` values returned by [`StreamingScope::read`].
    pub const STREAM_BITWIDTH: u32 = 16;

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
        Ok(ScopeReading::from_wire_bytes(effective_msps, &data)?)
    }

    pub fn stream(self) -> Result<StreamingScope, (Self, FleaTerminalError)> {
        let command = format!(
            "stream {} {} {}",
            Self::STREAM_SAMPLE_LENGTH,
            Self::STREAM_AVERAGING,
            Self::STREAM_BITWIDTH,
        );
        match self.serial.exec_async(&command) {
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

    /// Start a digital bit-banging stream.
    /// `n_samples_per_packet`: pre-agreed number of samples per packet; the
    /// firmware packs exactly this many samples into each flush with no inline
    /// header.  Use [`Self::compute_n_samples_per_packet`] to derive the value;
    /// it must be in `1..=504 / popcount(channel_mask)` so a packet fits the
    /// firmware's 63-byte buffer.
    pub fn digital_stream(
        self,
        channel_mask: u16,
        us_per_sample: u32,
        n_samples_per_packet: u32,
    ) -> Result<DigitalStreamingScope, (Self, CaptureConfigError)> {
        let packet_bytes = match Self::digital_packet_bytes(channel_mask, n_samples_per_packet) {
            Ok(packet_bytes) => packet_bytes,
            Err(e) => return Err((self, e)),
        };
        let command =
            format!("digital_stream 0x{channel_mask:x} {us_per_sample} {n_samples_per_packet}");
        match self.serial.exec_async(&command) {
            Ok(serial) => Ok(DigitalStreamingScope {
                _ver: self._ver,
                hostname: self.hostname,
                channel_mask,
                n_samples_per_packet: n_samples_per_packet as usize,
                packet_bytes,
                serial,
            }),
            Err((serial, e)) => Err((
                Self {
                    serial,
                    _ver: self._ver,
                    hostname: self.hostname,
                },
                e.into(),
            )),
        }
    }

    /// Bytes per digital-stream packet, after checking the parameters against
    /// the firmware limits (9 channels, one 63-byte USB FS packet).
    fn digital_packet_bytes(
        channel_mask: u16,
        n_samples_per_packet: u32,
    ) -> Result<usize, CaptureConfigError> {
        if channel_mask == 0 || channel_mask > 0x1ff {
            return Err(CaptureConfigError::InvalidChannelMask(channel_mask));
        }
        let n_channels = channel_mask.count_ones();
        let max = Self::DIGITAL_PACKET_BITS / n_channels;
        if n_samples_per_packet == 0 || n_samples_per_packet > max {
            return Err(CaptureConfigError::InvalidSamplesPerPacket {
                requested: n_samples_per_packet,
                max,
            });
        }
        Ok((n_samples_per_packet * n_channels).div_ceil(8) as usize)
    }

    /// Compute the packet sample count that balances throughput vs host latency.
    /// - Free-run (`us_per_sample == 0`): fill a full 63-byte USB FS packet.
    /// - Configured rate: target ~50 ms per packet (T3 clamps to ≥114 Hz).
    pub fn compute_n_samples_per_packet(channel_mask: u16, us_per_sample: u32) -> u32 {
        let n_channels = channel_mask.count_ones().max(1);
        let max_samples = Self::DIGITAL_PACKET_BITS / n_channels;
        if us_per_sample == 0 {
            return max_samples;
        }
        // T3 timer clamps at us_per_sample > 8738 → ~114 Hz actual.
        let actual_rate_hz: u32 = if us_per_sample > 8738 {
            114
        } else {
            1_000_000 / us_per_sample
        };
        let samples_50ms = (actual_rate_hz * 50 / 1000).max(1);
        samples_50ms.min(max_samples)
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

pub struct DigitalStreamingScope {
    _ver: String,
    hostname: String,
    pub channel_mask: u16,
    /// Pre-agreed samples per packet — no inline header in the wire format.
    pub n_samples_per_packet: usize,
    /// Bytes per packet = `ceil(n_samples_per_packet * popcount(channel_mask) / 8)`.
    packet_bytes: usize,
    serial: BusyFleaTerminal,
}

impl DigitalStreamingScope {
    pub fn stop(self) -> Result<IdleFleaScope, FleaTerminalError> {
        let serial = self.serial.cancel()?;
        Ok(IdleFleaScope {
            serial,
            _ver: self._ver,
            hostname: self.hostname,
        })
    }

    /// Read one packet and decode it into per-sample bitmasks.
    /// Reads exactly `packet_bytes` bytes (no header) and decodes
    /// exactly `n_samples_per_packet` samples — zero-padded trailing bits
    /// in the last byte are never decoded.
    pub fn read_packet(&mut self) -> Result<Vec<u16>, std::io::Error> {
        profiling::scope!("DigitalStreamingScope::read_packet");
        let mut buffer = vec![0u8; self.packet_bytes];
        self.serial.read_exact(&mut buffer)?;
        Ok(Self::decode_packet(
            &buffer,
            self.channel_mask,
            self.n_samples_per_packet,
        ))
    }

    /// Unpack the firmware's LSB-first bit stream: sample `s`, active channel
    /// `k` (in ascending channel order) is bit `s * popcount(mask) + k`.
    fn decode_packet(buffer: &[u8], channel_mask: u16, n_samples: usize) -> Vec<u16> {
        let n_channels = channel_mask.count_ones() as usize;
        let active: Vec<usize> = (0..9).filter(|&i| channel_mask & (1 << i) != 0).collect();
        let bit_set = |bit_idx: usize| {
            buffer
                .get(bit_idx / 8)
                .is_some_and(|byte| (byte >> (bit_idx % 8)) & 1 != 0)
        };
        (0..n_samples)
            .map(|s| {
                active
                    .iter()
                    .enumerate()
                    .filter(|&(ch_idx, _)| bit_set(s * n_channels + ch_idx))
                    .fold(0u16, |mask, (_, &virt_ch)| mask | (1 << virt_ch))
            })
            .collect()
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
    fn digital_packet_bytes_enforces_firmware_limits() {
        use CaptureConfigError::{InvalidChannelMask, InvalidSamplesPerPacket};
        let bytes = IdleFleaScope::digital_packet_bytes;
        assert!(matches!(bytes(0, 1), Err(InvalidChannelMask(0))));
        assert!(matches!(bytes(0x200, 1), Err(InvalidChannelMask(0x200))));
        // 9 channels: at most 504 / 9 = 56 samples fit 63 bytes.
        assert_eq!(bytes(0x1ff, 56).unwrap(), 63);
        assert!(matches!(
            bytes(0x1ff, 57),
            Err(InvalidSamplesPerPacket {
                requested: 57,
                max: 56
            })
        ));
        assert!(matches!(
            bytes(0x1, 0),
            Err(InvalidSamplesPerPacket {
                requested: 0,
                max: 504
            })
        ));
        // 3 channels × 5 samples = 15 bits → 2 bytes.
        assert_eq!(bytes(0b101_0001, 5).unwrap(), 2);
    }

    #[test]
    fn compute_n_samples_per_packet_stays_within_limits() {
        let n = IdleFleaScope::compute_n_samples_per_packet;
        // Free-run fills the packet.
        assert_eq!(n(0x1ff, 0), 56);
        assert_eq!(n(0x1, 0), 504);
        // 1 kHz → 50 samples in 50 ms.
        assert_eq!(n(0x1, 1000), 50);
        // Capped by the packet size.
        assert_eq!(n(0x1ff, 10), 56);
        // Slower than the T3 clamp (> 8738 µs) runs at ~114 Hz → 5 samples.
        assert_eq!(n(0x1, 100_000), 5);
        // Never 0, and always accepted by digital_stream.
        for (mask, us) in [(0x1, 1_000_000), (0x1ff, 0), (0x1ff, 8738), (0x3, 1)] {
            assert!(IdleFleaScope::digital_packet_bytes(mask, n(mask, us)).is_ok());
        }
    }

    #[test]
    fn decode_packet_maps_lsb_first_bits_to_sparse_channels() {
        // Channels 0, 4 and 8 active; firmware packs (ch0, ch4, ch8) per
        // sample, LSB first. Samples: {ch0}, {ch4, ch8}, {all}, {}.
        // Bit stream (index 0 first): 1 0 0 | 0 1 1 | 1 1 1 | 0 0 0
        // → byte 0 = bits 0..=7 = 0b1111_0001, byte 1 = bit 8 = 0b0000_0001.
        let decoded = DigitalStreamingScope::decode_packet(&[0b1111_0001, 0b0000_0001], 0x111, 4);
        assert_eq!(decoded, [0x001, 0x110, 0x111, 0x000]);
    }

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
}
