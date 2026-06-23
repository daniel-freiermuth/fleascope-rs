use serialport::SerialPort;
use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};


/// Progress information passed to the `on_progress` callback during [`IdleFleaTerminal::flash_upgrade`].
#[derive(Debug, Clone, Copy)]
pub struct FlashProgress {
    /// Number of records sent so far.
    pub sent: usize,
    /// Total number of records to send.
    pub total: usize,
}

/// Errors that can occur during a firmware flash upgrade.
#[derive(Debug, thiserror::Error)]
pub enum FlashUpgradeError {
    #[error("Serial terminal error: {0}")]
    Terminal(#[from] FleaTerminalError),

    #[error("Device did not start upgrade within timeout; no 'paste HEX upgrade file now' received")]
    UpgradeStartTimeout,

    #[error("Upgrade failed — device output: {0}")]
    UpgradeFailed(String),
}

const PROMPT: &[u8] = b"> ";

#[derive(Debug)]
pub struct StatelessFleaTerminal {
    serial: Box<dyn SerialPort>,
}

pub struct IdleFleaTerminal {
    inner: StatelessFleaTerminal,
}

pub struct ConnectionLostError;

#[derive(Debug, thiserror::Error)]
pub enum FleaTerminalError {
    #[error("Serial port error: {0}")]
    SerialPort(#[from] serialport::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Timeout error: Expected prompt within {timeout:?}.")]
    Timeout { timeout: Duration },

    #[error("Connection lost while waiting for response")]
    ConnectionLost,
}

impl StatelessFleaTerminal {
    /// Create a new `FleaTerminal` instance
    pub fn new(port: &str) -> Result<Self, FleaTerminalError> {
        profiling::scope!("StatelessFleaTerminal::new");

        let serial = serialport::new(port, 9600)
            .timeout(Duration::from_millis(70))
            .open()?;

        let mut terminal = Self { serial };

        terminal.flush()?;
        Ok(terminal)
    }

    /// Flush the serial buffer
    fn flush(&mut self) -> Result<(), FleaTerminalError> {
        log::debug!("Flushing serial port buffers once");
        self.serial.clear(serialport::ClearBuffer::All)?;
        while self.serial.bytes_to_read().unwrap() > 0 {
            log::debug!("Flushing serial port buffers twice");
            self.serial.clear(serialport::ClearBuffer::Input)?;
        }
        loop {
            let mut buf = [0u8; 1024];
            match self.serial.read(&mut buf) {
                Ok(n) => {
                    if n == 0 {
                        break;
                    }
                    log::debug!("Flushing serial port buffers thrice");
                }
                Err(e) if e.kind() == ErrorKind::TimedOut => break,
                Err(e) => return Err(FleaTerminalError::Io(e)),
            }
        }
        Ok(())
    }

    fn read_chunk(&mut self, response: &mut Vec<u8>) -> Result<bool, FleaTerminalError> {
        let mut read_buffer = [0u8; 1024]; // Read in chunks
        profiling::scope!("read_chunk");
        match self.serial.read(&mut read_buffer) {
            Ok(bytes_read) if bytes_read > 0 => {
                profiling::scope!("process_chunk_data");

                response.extend_from_slice(&read_buffer[..bytes_read]);

                // Check if we have the prompt at the end
                if response.len() >= PROMPT.len() {
                    let potential_prompt = &response[response.len() - PROMPT.len()..];
                    if potential_prompt == PROMPT {
                        Ok(true)
                    } else {
                        Ok(false)
                    }
                } else {
                    Ok(false)
                }
            }
            Ok(_) => {
                // No data available right now, but no error
                Ok(false)
            }
            Err(e) if e.kind() == ErrorKind::TimedOut => {
                // Timeout is expected in non-blocking reads
                Ok(false)
            }
            Err(e) if e.kind() == ErrorKind::BrokenPipe
                || e.kind() == ErrorKind::UnexpectedEof =>
            {
                Err(FleaTerminalError::ConnectionLost)
            }
            Err(e) => {
                tracing::info!("Serial read error (kind: {:?})...{e}", e.kind());
                Err(FleaTerminalError::Io(e))
            }
        }
    }

    fn exec_sync(
        &mut self,
        command: &str,
        timeout: Option<Duration>,
    ) -> Result<Vec<u8>, FleaTerminalError> {
        profiling::scope!("exec_sync");

        {
            profiling::scope!("serial_write_command");
            // Send command
            let command_with_newline = format!("{command}\n");
            self.serial.write_all(command_with_newline.as_bytes())?;
        }

        // Read response until prompt
        profiling::scope!("serial_read_response");

        let mut response = Vec::new();
        let now = Instant::now();

        loop {
            profiling::scope!("serial_read_chunk");
            match self.read_chunk(&mut response) {
                Ok(true) => break,
                Ok(false) => {}
                Err(e) => return Err(e),
            }
            if let Some(t) = timeout {
                if now.elapsed() >= t {
                    return Err(FleaTerminalError::Timeout { timeout: t });
                }
            }
        }

        // Remove the prompt from the end and convert to string
        let response_without_prompt = &response[..response.len() - PROMPT.len()];

        Ok(response_without_prompt.to_vec())
    }

    /// Send CTRL-C character
    pub fn send_ctrl_c(&mut self) -> Result<(), FleaTerminalError> {
        self.serial.write_all(&[0x03])?;
        Ok(())
    }

    /// Send reset command
    pub fn send_reset(&mut self) -> Result<(), FleaTerminalError> {
        self.serial.write_all(b"reset\n")?;
        Ok(())
    }

    /// Write raw bytes directly to the serial port.
    pub(crate) fn write_raw(&mut self, data: &[u8]) -> Result<(), FleaTerminalError> {
        self.serial.write_all(data)?;
        Ok(())
    }

    /// Read bytes until one of `patterns` is found as a contiguous substring of the response,
    /// or until `timeout` elapses.
    ///
    /// Returns `(accumulated_bytes, index_of_matched_pattern)`.
    /// Returns `Err(FleaTerminalError::Timeout { … })` if no pattern matched before the deadline.
    pub(crate) fn read_until_pattern(
        &mut self,
        patterns: &[&[u8]],
        timeout: Duration,
    ) -> Result<(Vec<u8>, usize), FleaTerminalError> {
        let mut response: Vec<u8> = Vec::new();
        let start = Instant::now();

        loop {
            let mut buf = [0u8; 1024];
            match self.serial.read(&mut buf) {
                Ok(n) if n > 0 => {
                    response.extend_from_slice(&buf[..n]);
                    for (idx, pattern) in patterns.iter().enumerate() {
                        if response.windows(pattern.len()).any(|w| w == *pattern) {
                            return Ok((response, idx));
                        }
                    }
                }
                Ok(_) => {
                    // Zero-byte read from non-blocking serial; treat as timeout.
                }
                Err(ref e) if e.kind() == ErrorKind::TimedOut => {
                    // Normal non-blocking timeout; check our deadline.
                }
                Err(ref e)
                    if e.kind() == ErrorKind::BrokenPipe
                        || e.kind() == ErrorKind::UnexpectedEof =>
                {
                    return Err(FleaTerminalError::ConnectionLost);
                }
                Err(e) => {
                    tracing::warn!("Serial read error during pattern wait: {e}");
                    return Err(FleaTerminalError::Io(e));
                }
            }
            if start.elapsed() >= timeout {
                return Err(FleaTerminalError::Timeout { timeout });
            }
        }
    }
}

impl IdleFleaTerminal {
    pub fn exec_async(mut self, command: &str) -> BusyFleaTerminal {
        profiling::scope!("IdleFleaTerminal::exec_async");

        let command_with_newline = format!("{command}\n");
        self.inner
            .serial
            .write_all(command_with_newline.as_bytes())
            .expect("Failed to write command to serial port");

        BusyFleaTerminal {
            inner: self.inner,
            response: Vec::new(),
        }
    }
    pub fn exec_sync(&mut self, command: &str, timeout: Option<Duration>) -> Vec<u8> {
        profiling::scope!("IdleFleaTerminal::exec_sync");

        self.inner
            .exec_sync(command, timeout)
            .expect("Failed to execute command")
    }

    /// Flash new firmware to the device by streaming Intel HEX records.
    ///
    /// Steps:
    /// 1. Sends the `upgrade` command.
    /// 2. Waits for the device to prompt for HEX input.
    /// 3. Streams every record line to the device.
    /// 4. Waits for `"paste done!"` (success) or `"upgrade failed"` (failure).
    ///
    /// `on_progress` is called after each record line is sent with current progress.
    /// After a successful flash the device resets itself; the connection will be dead.
    pub fn flash_upgrade(
        &mut self,
        hex_lines: &[&str],
        mut on_progress: impl FnMut(FlashProgress),
    ) -> Result<(), FlashUpgradeError> {
        let total = hex_lines.len();

        // 1. Send the upgrade command directly (not via exec_sync — the device will NOT
        //    return a `> ` prompt during the upgrade; it just waits for HEX records).
        self.inner.write_raw(b"upgrade\n")?;

        // 2. Wait for the device to announce it is ready for HEX input.
        let ready = self
            .inner
            .read_until_pattern(&[b"paste HEX upgrade file now"], Duration::from_secs(5));
        match ready {
            Ok(_) => {}
            Err(FleaTerminalError::Timeout { .. }) => {
                return Err(FlashUpgradeError::UpgradeStartTimeout);
            }
            Err(e) => return Err(FlashUpgradeError::Terminal(e)),
        }

        // 3. Stream every HEX record.
        for (i, line) in hex_lines.iter().enumerate() {
            let record = format!("{line}\n");
            self.inner.write_raw(record.as_bytes())?;
            on_progress(FlashProgress {
                sent: i + 1,
                total,
            });
        }

        // 4. Wait for completion.  The device resets after "paste done!" so we may
        //    never see the closing `> ` prompt — that is expected.
        let (output, matched) = self.inner.read_until_pattern(
            &[b"paste done!", b"upgrade failed"],
            Duration::from_secs(120),
        )?;

        if matched == 1 {
            let msg = String::from_utf8_lossy(&output).into_owned();
            return Err(FlashUpgradeError::UpgradeFailed(msg));
        }

        Ok(())
    }
}
impl TryFrom<StatelessFleaTerminal> for IdleFleaTerminal {
    type Error = (StatelessFleaTerminal, FleaTerminalError);

    fn try_from(mut value: StatelessFleaTerminal) -> Result<Self, Self::Error> {
        profiling::scope!("IdleFleaTerminal::try_from");

        log::debug!("Connected to FleaScope. Sending CTRL-C to reset.");
        match value.send_ctrl_c() {
            Ok(()) => {}
            Err(e) => return Err((value, e)),
        }
        if let Err(e) = value.flush() {
            return Err((value, e));
        }

        log::debug!("Turning on prompt");
        if let Err(e) = value.exec_sync("prompt on", Some(Duration::from_secs(1))) {
            return Err((value, e));
        }

        if let Err(e) = value.flush() {
            return Err((value, e));
        }
        Ok(Self { inner: value })
    }
}

pub struct BusyFleaTerminal {
    inner: StatelessFleaTerminal,
    response: Vec<u8>,
}

impl BusyFleaTerminal {
    pub fn cancel(mut self) -> IdleFleaTerminal {
        self.inner.send_ctrl_c().expect("Failed to send CTRL-C");
        const PROMPT_LEN: usize = PROMPT.len();
        const BUFFER_LEN: usize = 1024;
        let mut prompt_buffer = VecDeque::with_capacity(PROMPT_LEN);
        let mut read_buffer = [0u8; BUFFER_LEN];
        loop {
            match self.inner.serial.read(&mut read_buffer) {
                Ok(bytes_read) if bytes_read >= PROMPT_LEN => {
                    prompt_buffer =
                        VecDeque::from(read_buffer[bytes_read - PROMPT_LEN..bytes_read].to_vec());
                }
                Ok(bytes_read) if bytes_read > 0 => {
                    for _i in 0..bytes_read {
                        prompt_buffer.pop_front();
                    }
                    prompt_buffer.extend(&read_buffer[..bytes_read]);
                }
                Ok(_) => continue, // No data available right now, but no error
                Err(e) if e.kind() == ErrorKind::TimedOut => continue, // Timeout is expected in non-blocking reads
                // An unexpected (non-timeout) serial I/O error while draining after cancel.
                // `cancel()` returns `IdleFleaTerminal`, not a `Result`, so we cannot
                // propagate; panic is the only option here.
                #[allow(clippy::panic)]
                Err(e) => panic!("Serial read error in cancel: {e}"),
            }
            // Check if we have the prompt at the end
            if prompt_buffer.len() == PROMPT.len()
                && prompt_buffer.iter().copied().eq(PROMPT.iter().copied())
            {
                break;
            }
        }
        self.inner.flush().expect("Failed to flush serial port");
        IdleFleaTerminal { inner: self.inner }
    }

    fn into_result(self) -> (Vec<u8>, IdleFleaTerminal) {
        profiling::scope!("BusyFleaTerminal::into_result");

        // Remove the prompt from the end and convert to string
        let response_without_prompt = &self.response[..self.response.len() - PROMPT.len()];
        let response_str = response_without_prompt.to_vec();

        (response_str, IdleFleaTerminal { inner: self.inner })
    }

    pub fn try_get_result(
        mut self,
    ) -> Result<Result<(Vec<u8>, IdleFleaTerminal), Self>, ConnectionLostError> {
        profiling::scope!("BusyFleaTerminal::try_get_result");

        // There are 24000 bytes tranferred right now which takes 24ms at 1 MB/s
        // Capturing takes about 7ms, transfer around 30ms
        // Sleeping here leads to larger chunks, but there isn't really a benefit
        // Timing: capture time,
        //     7ms whatever on device (increasing with capture time),
        //     30ms transfer
        // Possible improvements:
        // - Fix whatever takes so long on the device. Should be faster than 7ms
        // - Fix whatever takes increasing amount of time on the device
        // - Improve transfer speed by • encoding as bytes, • drop digital channels?
        // - Live sending of data. Seems like data is way faster than data transfer

        match self.inner.read_chunk(&mut self.response) {
            Ok(true) => Ok(Ok(self.into_result())),
            Ok(false) => Ok(Err(self)),
            Err(_) => Err(ConnectionLostError),
        }
    }
}

impl Read for BusyFleaTerminal {
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, std::io::Error> {
        profiling::scope!("BusyFleaTerminal::read");

        self.inner.serial.read(buffer)
    }
}
