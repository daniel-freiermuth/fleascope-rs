use serialport::SerialPort;
use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

const PROMPT: &[u8] = b"> ";
/// How long `cancel()`, and a resync without a caller timeout, wait for the prompt.
const CANCEL_TIMEOUT: Duration = Duration::from_secs(5);
/// How long [`IdleFleaTerminal::flash_upgrade`] waits for the device to ask for HEX records.
const UPGRADE_START_TIMEOUT: Duration = Duration::from_secs(5);
/// How long [`IdleFleaTerminal::flash_upgrade`] waits for the device to finish after the last record.
const UPGRADE_FINISH_TIMEOUT: Duration = Duration::from_secs(120);
const UPGRADE_READY: &[u8] = b"paste HEX upgrade file now";
const UPGRADE_DONE: &[u8] = b"paste done!";
const UPGRADE_FAILED: &[u8] = b"upgrade failed";

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
#[non_exhaustive]
pub enum FlashUpgradeError {
    #[error("Serial terminal error: {0}")]
    Terminal(#[from] FleaTerminalError),

    #[error(
        "Device did not start upgrade within timeout; no 'paste HEX upgrade file now' received"
    )]
    UpgradeStartTimeout,

    #[error("Upgrade failed — device output: {0}")]
    UpgradeFailed(String),
}

#[derive(Debug)]
pub struct StatelessFleaTerminal {
    serial: Box<dyn SerialPort>,
}

pub struct IdleFleaTerminal {
    inner: StatelessFleaTerminal,
    /// A command failed and left the device in an unknown state; the next
    /// command must resync it to a fresh prompt before writing anything.
    needs_resync: bool,
}

pub struct ConnectionLostError;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FleaTerminalError {
    #[error("Serial port error: {0}")]
    SerialPort(#[from] serialport::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Timeout error: Expected prompt within {timeout:?}.")]
    Timeout { timeout: Duration },

    #[error("Connection lost while waiting for response")]
    ConnectionLost,

    #[error("Unexpected response to `{command}`: {response:?}")]
    UnexpectedResponse {
        command: Box<str>,
        response: Box<str>,
    },
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
        while self.serial.bytes_to_read()? > 0 {
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
                // A signal interrupted the read; nothing was read, so retry.
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
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

                response.extend(read_buffer.iter().take(bytes_read));

                // Check if we have the prompt at the end
                Ok(response.ends_with(PROMPT))
            }
            Ok(_) => {
                // No data available right now, but no error
                Ok(false)
            }
            Err(e) if e.kind() == ErrorKind::TimedOut => {
                // Timeout is expected in non-blocking reads
                Ok(false)
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {
                // A signal interrupted the read; nothing was read, the caller polls again
                Ok(false)
            }
            Err(e) if e.kind() == ErrorKind::BrokenPipe => Err(FleaTerminalError::ConnectionLost),
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                Err(FleaTerminalError::ConnectionLost)
            }
            Err(e) => Err(FleaTerminalError::Io(e)),
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
            if self.read_chunk(&mut response)? {
                break;
            }
            if let Some(t) = timeout
                && now.elapsed() >= t
            {
                return Err(FleaTerminalError::Timeout { timeout: t });
            }
        }

        Ok(strip_prompt_and_trim(&response))
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

    /// Idle-line resync. On StickOS, CTRL-C silently clears a partly typed line
    /// and prints no prompt (`sources/terminal.c` accumulate()); `"> "` only
    /// follows a command line. So CTRL-C is followed by an empty line, a no-op
    /// command that answers with a prompt. If an earlier command is still
    /// running, CTRL-C stops it as in `cancel()`.
    fn resync(&mut self, timeout: Duration) -> Result<(), FleaTerminalError> {
        self.serial.write_all(b"\x03\n")?;
        self.await_prompt_and_flush(timeout)
    }

    /// Wait for the prompt and flush, leaving nothing left to read.
    fn await_prompt_and_flush(&mut self, timeout: Duration) -> Result<(), FleaTerminalError> {
        let cancel_start = Instant::now();
        const PROMPT_LEN: usize = PROMPT.len();
        const BUFFER_LEN: usize = 1024;
        let mut prompt_buffer = VecDeque::with_capacity(PROMPT_LEN);
        let mut read_buffer = [0u8; BUFFER_LEN];
        loop {
            match self.serial.read(&mut read_buffer) {
                Ok(bytes_read) if bytes_read > 0 => {
                    prompt_window_extend(
                        &mut prompt_buffer,
                        read_buffer.iter().take(bytes_read),
                        PROMPT_LEN,
                    );
                }
                Ok(_) => {} // No data available right now, but no error
                Err(e) if e.kind() == ErrorKind::TimedOut => {} // Timeout is expected in non-blocking reads
                Err(e) if e.kind() == ErrorKind::Interrupted => {} // A signal interrupted the read; retry
                Err(e) if e.kind() == ErrorKind::BrokenPipe => {
                    return Err(FleaTerminalError::ConnectionLost);
                }
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                    return Err(FleaTerminalError::ConnectionLost);
                }
                Err(e) => return Err(FleaTerminalError::Io(e)),
            }
            // Check if we have the prompt at the end
            if prompt_buffer.len() == PROMPT.len()
                && prompt_buffer.iter().copied().eq(PROMPT.iter().copied())
            {
                break;
            }
            if cancel_start.elapsed() >= timeout {
                return Err(FleaTerminalError::Timeout { timeout });
            }
        }
        self.flush()
    }

    /// Write raw bytes directly to the serial port.
    fn write_raw(&mut self, data: &[u8]) -> Result<(), FleaTerminalError> {
        self.serial.write_all(data)?;
        Ok(())
    }

    /// Read until one of `patterns` occurs anywhere in the received bytes,
    /// also when it straddles two reads.
    ///
    /// Returns the received bytes and the index of the matched pattern, or
    /// `FleaTerminalError::Timeout` if none matched within `timeout`.
    fn read_until_pattern(
        &mut self,
        patterns: &[&[u8]],
        timeout: Duration,
    ) -> Result<(Vec<u8>, usize), FleaTerminalError> {
        let mut response = Vec::new();
        let start = Instant::now();
        loop {
            let searched = response.len();
            // Prompt detection is irrelevant here; read_chunk only extends `response`.
            self.read_chunk(&mut response)?;
            if response.len() > searched
                && let Some(idx) = patterns
                    .iter()
                    .position(|p| contains_from(&response, searched, p))
            {
                return Ok((response, idx));
            }
            if start.elapsed() >= timeout {
                return Err(FleaTerminalError::Timeout { timeout });
            }
        }
    }
}

impl IdleFleaTerminal {
    fn new(inner: StatelessFleaTerminal) -> Self {
        Self {
            inner,
            needs_resync: false,
        }
    }

    /// Resync to a fresh prompt if a failed command left that pending, so a
    /// command is never sent onto a stale line or read as an earlier reply.
    /// Waits for the prompt at most `timeout`, or `CANCEL_TIMEOUT` if `None`.
    fn ensure_synced(&mut self, timeout: Option<Duration>) -> Result<(), FleaTerminalError> {
        if self.needs_resync {
            self.inner.resync(timeout.unwrap_or(CANCEL_TIMEOUT))?;
            self.needs_resync = false;
        }
        Ok(())
    }

    pub fn exec_async(
        mut self,
        command: &str,
    ) -> Result<BusyFleaTerminal, (Self, FleaTerminalError)> {
        profiling::scope!("IdleFleaTerminal::exec_async");

        if let Err(e) = self.ensure_synced(None) {
            return Err((self, e));
        }
        let command_with_newline = format!("{command}\n");
        if let Err(e) = self.inner.serial.write_all(command_with_newline.as_bytes()) {
            // The device may hold a partly written line; resync before the next command.
            self.needs_resync = true;
            return Err((self, e.into()));
        }

        Ok(BusyFleaTerminal {
            inner: self.inner,
            response: Vec::new(),
        })
    }
    pub fn exec_sync(
        &mut self,
        command: &str,
        timeout: Option<Duration>,
    ) -> Result<Vec<u8>, FleaTerminalError> {
        profiling::scope!("IdleFleaTerminal::exec_sync");

        // A pending resync is bounded by `timeout` on its own, and the command
        // then gets the full `timeout` too. Sharing one deadline would leave
        // the command less time than on a healthy terminal (the resync's flush
        // alone takes a full port read timeout), so a short timeout would time
        // out, re-arm the resync, and never recover.
        self.ensure_synced(timeout)?;
        self.inner.exec_sync(command, timeout).inspect_err(|_| {
            // The device may hold a partly written line or still answer
            // later; resync before the next command.
            self.needs_resync = true;
        })
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

        self.ensure_synced(None)?;
        // From here on the device leaves the prompt-driven command loop: it
        // answers `upgrade` without a prompt and resets after a successful
        // flash. Whatever happens, the next command has to resync first.
        self.needs_resync = true;

        // 1. Send the upgrade command directly (not via exec_sync — the device will NOT
        //    return a `> ` prompt during the upgrade; it just waits for HEX records).
        self.inner.write_raw(b"upgrade\n")?;

        // 2. Wait for the device to announce it is ready for HEX input.
        match self
            .inner
            .read_until_pattern(&[UPGRADE_READY], UPGRADE_START_TIMEOUT)
        {
            Ok(_) => {}
            Err(FleaTerminalError::Timeout { .. }) => {
                return Err(FlashUpgradeError::UpgradeStartTimeout);
            }
            Err(e) => return Err(e.into()),
        }

        // 3. Stream every HEX record.
        let mut record = String::new();
        for (i, line) in hex_lines.iter().enumerate() {
            record.clear();
            record.push_str(line);
            record.push('\n');
            self.inner.write_raw(record.as_bytes())?;
            on_progress(FlashProgress { sent: i + 1, total });
        }

        // 4. Wait for completion. The device resets after "paste done!" so we may
        //    never see the closing `> ` prompt — that is expected.
        let (output, matched) = self
            .inner
            .read_until_pattern(&[UPGRADE_DONE, UPGRADE_FAILED], UPGRADE_FINISH_TIMEOUT)?;
        if matched == 1 {
            return Err(FlashUpgradeError::UpgradeFailed(
                String::from_utf8_lossy(&output).into_owned(),
            ));
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
        Ok(Self::new(value))
    }
}

pub struct BusyFleaTerminal {
    inner: StatelessFleaTerminal,
    response: Vec<u8>,
}

impl BusyFleaTerminal {
    pub fn cancel(mut self) -> Result<IdleFleaTerminal, FleaTerminalError> {
        self.inner.send_ctrl_c()?;
        self.inner.await_prompt_and_flush(CANCEL_TIMEOUT)?;
        Ok(IdleFleaTerminal::new(self.inner))
    }

    fn into_result(self) -> (Vec<u8>, IdleFleaTerminal) {
        profiling::scope!("BusyFleaTerminal::into_result");

        let trimmed = strip_prompt_and_trim(&self.response);
        (trimmed, IdleFleaTerminal::new(self.inner))
    }

    pub fn try_get_result(mut self) -> Result<TerminalStatus, ConnectionLostError> {
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
            Ok(true) => {
                let (data, idle) = self.into_result();
                Ok(TerminalStatus::Done(data, idle))
            }
            Ok(false) => Ok(TerminalStatus::Pending(self)),
            Err(FleaTerminalError::ConnectionLost) => Err(ConnectionLostError),
            Err(e) => {
                tracing::warn!("Serial read error, treating the connection as lost: {e}");
                Err(ConnectionLostError)
            }
        }
    }
}

/// Outcome of polling a [`BusyFleaTerminal`].
#[must_use]
pub enum TerminalStatus {
    /// The command is still running; poll again later.
    Pending(BusyFleaTerminal),
    /// The prompt arrived: trimmed response and the terminal, idle again.
    Done(Vec<u8>, IdleFleaTerminal),
}

impl Read for BusyFleaTerminal {
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, std::io::Error> {
        profiling::scope!("BusyFleaTerminal::read");

        self.inner.serial.read(buffer)
    }
}

/// Drop the trailing prompt, then trim ASCII whitespace (command echo, `\r\n`).
/// Callers only pass responses that end in `PROMPT`.
fn strip_prompt_and_trim(response: &[u8]) -> Vec<u8> {
    response
        .strip_suffix(PROMPT)
        .unwrap_or(response)
        .trim_ascii()
        .to_vec()
}

fn prompt_window_extend<'a>(
    buf: &mut VecDeque<u8>,
    new: impl IntoIterator<Item = &'a u8>,
    window_len: usize,
) {
    buf.extend(new);
    while buf.len() > window_len {
        buf.pop_front();
    }
}

/// Whether `needle` occurs in `haystack` in a match ending past `searched`,
/// i.e. one not already visible in `haystack[..searched]`.
fn contains_from(haystack: &[u8], searched: usize, needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    let start = searched.saturating_sub(needle.len() - 1);
    haystack
        .get(start..)
        .is_some_and(|tail| tail.windows(needle.len()).any(|w| w == needle))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    fn prompt_window_single_byte_reads() {
        let mut buf = VecDeque::new();
        prompt_window_extend(&mut buf, b">", PROMPT.len());
        prompt_window_extend(&mut buf, b" ", PROMPT.len());
        assert_eq!(buf.len(), PROMPT.len(), "must reach PROMPT_LEN");
        assert!(buf.iter().copied().eq(PROMPT.iter().copied()));
    }
    #[test]
    fn prompt_window_slides_through_junk() {
        let mut buf = VecDeque::new();
        for &b in b"XY> " {
            prompt_window_extend(&mut buf, std::slice::from_ref(&b), PROMPT.len());
        }
        assert_eq!(buf.len(), PROMPT.len());
        assert!(buf.iter().copied().eq(PROMPT.iter().copied()));
    }
    #[test]
    fn prompt_window_large_read() {
        let mut buf = VecDeque::new();
        prompt_window_extend(&mut buf, b"ok\r\n> ", PROMPT.len());
        assert!(buf.iter().copied().eq(PROMPT.iter().copied()));
    }
    #[test]
    fn prompt_window_straddles_reads() {
        let mut buf = VecDeque::new();
        prompt_window_extend(&mut buf, b"data>", PROMPT.len());
        prompt_window_extend(&mut buf, b" ", PROMPT.len());
        assert!(buf.iter().copied().eq(PROMPT.iter().copied()));
    }

    /// Serial port whose device never answers: every read times out,
    /// like the real port opened with a 70ms read timeout.
    struct SilentPort;

    impl Read for SilentPort {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_millis(10));
            Err(std::io::Error::new(ErrorKind::TimedOut, "silent device"))
        }
    }

    impl Write for SilentPort {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl SerialPort for SilentPort {
        fn name(&self) -> Option<String> {
            None
        }
        fn baud_rate(&self) -> serialport::Result<u32> {
            unimplemented!()
        }
        fn data_bits(&self) -> serialport::Result<serialport::DataBits> {
            unimplemented!()
        }
        fn flow_control(&self) -> serialport::Result<serialport::FlowControl> {
            unimplemented!()
        }
        fn parity(&self) -> serialport::Result<serialport::Parity> {
            unimplemented!()
        }
        fn stop_bits(&self) -> serialport::Result<serialport::StopBits> {
            unimplemented!()
        }
        fn timeout(&self) -> Duration {
            Duration::from_millis(10)
        }
        fn set_baud_rate(&mut self, _: u32) -> serialport::Result<()> {
            unimplemented!()
        }
        fn set_data_bits(&mut self, _: serialport::DataBits) -> serialport::Result<()> {
            unimplemented!()
        }
        fn set_flow_control(&mut self, _: serialport::FlowControl) -> serialport::Result<()> {
            unimplemented!()
        }
        fn set_parity(&mut self, _: serialport::Parity) -> serialport::Result<()> {
            unimplemented!()
        }
        fn set_stop_bits(&mut self, _: serialport::StopBits) -> serialport::Result<()> {
            unimplemented!()
        }
        fn set_timeout(&mut self, _: Duration) -> serialport::Result<()> {
            unimplemented!()
        }
        fn write_request_to_send(&mut self, _: bool) -> serialport::Result<()> {
            unimplemented!()
        }
        fn write_data_terminal_ready(&mut self, _: bool) -> serialport::Result<()> {
            unimplemented!()
        }
        fn read_clear_to_send(&mut self) -> serialport::Result<bool> {
            unimplemented!()
        }
        fn read_data_set_ready(&mut self) -> serialport::Result<bool> {
            unimplemented!()
        }
        fn read_ring_indicator(&mut self) -> serialport::Result<bool> {
            unimplemented!()
        }
        fn read_carrier_detect(&mut self) -> serialport::Result<bool> {
            unimplemented!()
        }
        fn bytes_to_read(&self) -> serialport::Result<u32> {
            Ok(0)
        }
        fn bytes_to_write(&self) -> serialport::Result<u32> {
            Ok(0)
        }
        fn clear(&self, _: serialport::ClearBuffer) -> serialport::Result<()> {
            Ok(())
        }
        fn try_clone(&self) -> serialport::Result<Box<dyn SerialPort>> {
            unimplemented!()
        }
        fn set_break(&self) -> serialport::Result<()> {
            unimplemented!()
        }
        fn clear_break(&self) -> serialport::Result<()> {
            unimplemented!()
        }
    }

    #[test]
    fn cancel_times_out_when_device_stays_silent() {
        let busy = BusyFleaTerminal {
            inner: StatelessFleaTerminal {
                serial: Box::new(SilentPort),
            },
            response: Vec::new(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(busy.cancel().err());
        });
        // cancel() must give up after its 5s CANCEL_TIMEOUT; allow generous slack.
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Some(FleaTerminalError::Timeout { timeout })) => {
                assert_eq!(timeout, Duration::from_secs(5));
            }
            Ok(other) => panic!("expected Timeout error, got {other:?}"),
            Err(_) => panic!("cancel() did not return within 15s on a silent device"),
        }
    }

    /// Serial port as `serialport` reports it after a USB unplug: once the
    /// tty is hung up, `poll` sees POLLHUP so read/write fail with
    /// `BrokenPipe`, and `tcflush` fails with EIO (`ErrorKind::Unknown`).
    /// `pending` bytes are delivered before reads start failing, and
    /// `writable` keeps writes alive to model an unplug that happens after
    /// the last successful write. Once a read has seen the hangup, writes
    /// fail too, as they do on a hung-up tty. `read_error` is the kind the
    /// failing reads report: `BrokenPipe` for the hangup, anything else for
    /// a read error the terminal does not classify.
    struct UnpluggedPort {
        writable: bool,
        pending: Vec<u8>,
        read_error: ErrorKind,
    }

    impl Read for UnpluggedPort {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pending.is_empty() {
                self.writable = false;
                return Err(self.read_error.into());
            }
            let n = self.pending.len().min(buf.len());
            buf[..n].copy_from_slice(&self.pending[..n]);
            self.pending.drain(..n);
            Ok(n)
        }
    }

    impl Write for UnpluggedPort {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.writable {
                Ok(buf.len())
            } else {
                Err(ErrorKind::BrokenPipe.into())
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl SerialPort for UnpluggedPort {
        fn clear(&self, _: serialport::ClearBuffer) -> serialport::Result<()> {
            Err(serialport::Error::new(
                serialport::ErrorKind::Unknown,
                "I/O error",
            ))
        }
        fn bytes_to_read(&self) -> serialport::Result<u32> {
            Err(serialport::Error::new(
                serialport::ErrorKind::Unknown,
                "I/O error",
            ))
        }
        fn name(&self) -> Option<String> {
            unreachable!()
        }
        fn baud_rate(&self) -> serialport::Result<u32> {
            unreachable!()
        }
        fn data_bits(&self) -> serialport::Result<serialport::DataBits> {
            unreachable!()
        }
        fn flow_control(&self) -> serialport::Result<serialport::FlowControl> {
            unreachable!()
        }
        fn parity(&self) -> serialport::Result<serialport::Parity> {
            unreachable!()
        }
        fn stop_bits(&self) -> serialport::Result<serialport::StopBits> {
            unreachable!()
        }
        fn timeout(&self) -> Duration {
            unreachable!()
        }
        fn set_baud_rate(&mut self, _: u32) -> serialport::Result<()> {
            unreachable!()
        }
        fn set_data_bits(&mut self, _: serialport::DataBits) -> serialport::Result<()> {
            unreachable!()
        }
        fn set_flow_control(&mut self, _: serialport::FlowControl) -> serialport::Result<()> {
            unreachable!()
        }
        fn set_parity(&mut self, _: serialport::Parity) -> serialport::Result<()> {
            unreachable!()
        }
        fn set_stop_bits(&mut self, _: serialport::StopBits) -> serialport::Result<()> {
            unreachable!()
        }
        fn set_timeout(&mut self, _: Duration) -> serialport::Result<()> {
            unreachable!()
        }
        fn write_request_to_send(&mut self, _: bool) -> serialport::Result<()> {
            unreachable!()
        }
        fn write_data_terminal_ready(&mut self, _: bool) -> serialport::Result<()> {
            unreachable!()
        }
        fn read_clear_to_send(&mut self) -> serialport::Result<bool> {
            unreachable!()
        }
        fn read_data_set_ready(&mut self) -> serialport::Result<bool> {
            unreachable!()
        }
        fn read_ring_indicator(&mut self) -> serialport::Result<bool> {
            unreachable!()
        }
        fn read_carrier_detect(&mut self) -> serialport::Result<bool> {
            unreachable!()
        }
        fn bytes_to_write(&self) -> serialport::Result<u32> {
            unreachable!()
        }
        fn try_clone(&self) -> serialport::Result<Box<dyn SerialPort>> {
            unreachable!()
        }
        fn set_break(&self) -> serialport::Result<()> {
            unreachable!()
        }
        fn clear_break(&self) -> serialport::Result<()> {
            unreachable!()
        }
    }

    fn busy_terminal(port: UnpluggedPort) -> BusyFleaTerminal {
        BusyFleaTerminal {
            inner: StatelessFleaTerminal {
                serial: Box::new(port),
            },
            response: Vec::new(),
        }
    }

    #[test]
    fn cancel_on_unplugged_port_returns_error() {
        let busy = busy_terminal(UnpluggedPort {
            writable: false,
            pending: Vec::new(),
            read_error: ErrorKind::BrokenPipe,
        });
        match busy.cancel() {
            Err(FleaTerminalError::Io(e)) => assert_eq!(e.kind(), ErrorKind::BrokenPipe),
            Err(e) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("cancel() succeeded on an unplugged port"),
        }
    }

    #[test]
    fn cancel_unplugged_after_prompt_returns_error() {
        let busy = busy_terminal(UnpluggedPort {
            writable: true,
            pending: PROMPT.to_vec(),
            read_error: ErrorKind::BrokenPipe,
        });
        match busy.cancel() {
            Err(FleaTerminalError::SerialPort(_)) => {}
            Err(e) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("cancel() succeeded on an unplugged port"),
        }
    }

    fn idle_terminal(port: UnpluggedPort) -> IdleFleaTerminal {
        IdleFleaTerminal::new(StatelessFleaTerminal {
            serial: Box::new(port),
        })
    }

    #[test]
    fn exec_async_on_unplugged_port_returns_error() {
        let idle = idle_terminal(UnpluggedPort {
            writable: false,
            pending: Vec::new(),
            read_error: ErrorKind::BrokenPipe,
        });
        match idle.exec_async("read 1") {
            Err((_, FleaTerminalError::Io(e))) => assert_eq!(e.kind(), ErrorKind::BrokenPipe),
            Err((_, e)) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("exec_async() succeeded on an unplugged port"),
        }
    }

    #[test]
    fn exec_sync_on_unplugged_port_returns_error() {
        let mut idle = idle_terminal(UnpluggedPort {
            writable: false,
            pending: Vec::new(),
            read_error: ErrorKind::BrokenPipe,
        });
        match idle.exec_sync("ver", None) {
            Err(FleaTerminalError::Io(e)) => assert_eq!(e.kind(), ErrorKind::BrokenPipe),
            Err(e) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("exec_sync() succeeded on an unplugged port"),
        }
    }

    #[test]
    fn exec_sync_unplugged_before_response_returns_connection_lost() {
        let mut idle = idle_terminal(UnpluggedPort {
            writable: true,
            pending: Vec::new(),
            read_error: ErrorKind::BrokenPipe,
        });
        match idle.exec_sync("ver", None) {
            Err(FleaTerminalError::ConnectionLost) => {}
            Err(e) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("exec_sync() succeeded on an unplugged port"),
        }
    }

    #[test]
    fn unclassified_read_error_in_cancel_returns_io_error() {
        let busy = busy_terminal(UnpluggedPort {
            writable: true,
            pending: Vec::new(),
            read_error: ErrorKind::Other,
        });
        match busy.cancel() {
            Err(FleaTerminalError::Io(e)) => assert_eq!(e.kind(), ErrorKind::Other),
            Err(e) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("cancel() succeeded despite a failing read"),
        }
    }

    #[test]
    fn unclassified_read_error_in_exec_sync_returns_io_error() {
        let mut idle = idle_terminal(UnpluggedPort {
            writable: true,
            pending: Vec::new(),
            read_error: ErrorKind::Other,
        });
        match idle.exec_sync("ver", None) {
            Err(FleaTerminalError::Io(e)) => assert_eq!(e.kind(), ErrorKind::Other),
            Err(e) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("exec_sync() succeeded despite a failing read"),
        }
    }

    #[test]
    fn unclassified_read_error_in_try_get_result_reports_connection_lost() {
        let busy = busy_terminal(UnpluggedPort {
            writable: true,
            pending: Vec::new(),
            read_error: ErrorKind::Other,
        });
        assert!(matches!(busy.try_get_result(), Err(ConnectionLostError)));
    }

    /// Line-based fake FleaScope modelled on the StickOS terminal. Answers
    /// `ver` and `hostname`, answers any other line (including an empty one)
    /// with a bare prompt, and on CTRL-C silently drops the current line
    /// without printing a prompt (`sources/terminal.c` accumulate(), echo
    /// off; `"> "` only follows a command line). `ver_stall` reads time
    /// out before the `ver` reply shows up (a slow device). `write_budget`
    /// lets that many bytes through, then the next `failed_writes` writes time
    /// out (a stalled port), so the device holds a partly written line.
    /// `empty_read` is how long a read with nothing to deliver blocks (the
    /// real port's 70ms timeout); `line_reads` hands out at most one line per
    /// read, so a reply's output and its trailing prompt arrive separately.
    /// `bytes_to_read_fails` makes `bytes_to_read()` fail while `clear()`
    /// still succeeds. `interrupted_reads` makes that many reads fail with
    /// `Interrupted` first, as a real port read does when a signal handler
    /// runs during its `ppoll` (EINTR). `upgrade` asks for HEX records
    /// without a prompt; HEX records get no reply, except the end-of-file
    /// record (`paste done!`) and `:BAD` (`upgrade failed`). `max_read` caps
    /// the bytes per read, so a message can straddle reads. `ver_reply` is
    /// what `ver` prints before its prompt.
    pub(crate) struct FakeDevice {
        line: Vec<u8>,
        replies: std::cell::RefCell<VecDeque<u8>>,
        stall: usize,
        ver_stall: usize,
        write_budget: Option<usize>,
        failed_writes: usize,
        empty_read: Duration,
        line_reads: bool,
        bytes_to_read_fails: bool,
        interrupted_reads: usize,
        max_read: usize,
        pub(crate) ver_reply: &'static str,
    }

    impl FakeDevice {
        pub(crate) fn new(
            ver_stall: usize,
            write_budget: Option<usize>,
            failed_writes: usize,
        ) -> Self {
            Self {
                line: Vec::new(),
                replies: std::cell::RefCell::new(VecDeque::new()),
                stall: 0,
                ver_stall,
                write_budget,
                failed_writes,
                empty_read: Duration::from_millis(5),
                line_reads: false,
                bytes_to_read_fails: false,
                interrupted_reads: 0,
                max_read: usize::MAX,
                ver_reply: "1.0",
            }
        }
    }

    impl Read for FakeDevice {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.interrupted_reads > 0 {
                self.interrupted_reads -= 1;
                return Err(ErrorKind::Interrupted.into());
            }
            let mut replies = self.replies.borrow_mut();
            if self.stall > 0 || replies.is_empty() {
                self.stall = self.stall.saturating_sub(1);
                std::thread::sleep(self.empty_read);
                return Err(ErrorKind::TimedOut.into());
            }
            let mut n = replies.len().min(buf.len()).min(self.max_read);
            if self.line_reads
                && let Some(end) = replies.iter().position(|&b| b == b'\n')
            {
                n = n.min(end + 1);
            }
            for (dst, src) in buf.iter_mut().zip(replies.drain(..n)) {
                *dst = src;
            }
            Ok(n)
        }
    }

    impl Write for FakeDevice {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut n = buf.len();
            if let Some(budget) = self.write_budget {
                if budget == 0 {
                    self.failed_writes = self.failed_writes.saturating_sub(1);
                    if self.failed_writes == 0 {
                        self.write_budget = None;
                    }
                    return Err(ErrorKind::TimedOut.into());
                }
                n = n.min(budget);
                self.write_budget = Some(budget - n);
            }
            let mut replies = self.replies.borrow_mut();
            for &b in &buf[..n] {
                match b {
                    0x03 => self.line.clear(),
                    b'\n' => {
                        let ver_reply;
                        let reply: &[u8] = match self.line.as_slice() {
                            b"ver" => {
                                self.stall = self.ver_stall;
                                ver_reply = format!("{}\r\n> ", self.ver_reply);
                                ver_reply.as_bytes()
                            }
                            b"hostname" => b"smoke\r\n> ",
                            b"upgrade" => b"paste HEX upgrade file now\r\n",
                            b":00000001FF" => b"paste done!\r\n",
                            b":BAD" => b"upgrade failed\r\n> ",
                            l if l.starts_with(b":") => b"",
                            _ => b"> ",
                        };
                        replies.extend(reply);
                        self.line.clear();
                    }
                    _ => self.line.push(b),
                }
            }
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl SerialPort for FakeDevice {
        fn clear(&self, _: serialport::ClearBuffer) -> serialport::Result<()> {
            self.replies.borrow_mut().clear();
            Ok(())
        }
        fn bytes_to_read(&self) -> serialport::Result<u32> {
            if self.bytes_to_read_fails {
                return Err(serialport::Error::new(
                    serialport::ErrorKind::Unknown,
                    "I/O error",
                ));
            }
            Ok(self.replies.borrow().len() as u32)
        }
        fn name(&self) -> Option<String> {
            unreachable!()
        }
        fn baud_rate(&self) -> serialport::Result<u32> {
            unreachable!()
        }
        fn data_bits(&self) -> serialport::Result<serialport::DataBits> {
            unreachable!()
        }
        fn flow_control(&self) -> serialport::Result<serialport::FlowControl> {
            unreachable!()
        }
        fn parity(&self) -> serialport::Result<serialport::Parity> {
            unreachable!()
        }
        fn stop_bits(&self) -> serialport::Result<serialport::StopBits> {
            unreachable!()
        }
        fn timeout(&self) -> Duration {
            unreachable!()
        }
        fn set_baud_rate(&mut self, _: u32) -> serialport::Result<()> {
            unreachable!()
        }
        fn set_data_bits(&mut self, _: serialport::DataBits) -> serialport::Result<()> {
            unreachable!()
        }
        fn set_flow_control(&mut self, _: serialport::FlowControl) -> serialport::Result<()> {
            unreachable!()
        }
        fn set_parity(&mut self, _: serialport::Parity) -> serialport::Result<()> {
            unreachable!()
        }
        fn set_stop_bits(&mut self, _: serialport::StopBits) -> serialport::Result<()> {
            unreachable!()
        }
        fn set_timeout(&mut self, _: Duration) -> serialport::Result<()> {
            unreachable!()
        }
        fn write_request_to_send(&mut self, _: bool) -> serialport::Result<()> {
            unreachable!()
        }
        fn write_data_terminal_ready(&mut self, _: bool) -> serialport::Result<()> {
            unreachable!()
        }
        fn read_clear_to_send(&mut self) -> serialport::Result<bool> {
            unreachable!()
        }
        fn read_data_set_ready(&mut self) -> serialport::Result<bool> {
            unreachable!()
        }
        fn read_ring_indicator(&mut self) -> serialport::Result<bool> {
            unreachable!()
        }
        fn read_carrier_detect(&mut self) -> serialport::Result<bool> {
            unreachable!()
        }
        fn bytes_to_write(&self) -> serialport::Result<u32> {
            unreachable!()
        }
        fn try_clone(&self) -> serialport::Result<Box<dyn SerialPort>> {
            unreachable!()
        }
        fn set_break(&self) -> serialport::Result<()> {
            unreachable!()
        }
        fn clear_break(&self) -> serialport::Result<()> {
            unreachable!()
        }
    }

    pub(crate) fn idle_fake_device(device: FakeDevice) -> IdleFleaTerminal {
        IdleFleaTerminal::new(StatelessFleaTerminal {
            serial: Box::new(device),
        })
    }

    #[test]
    fn exec_sync_timeout_does_not_leak_late_reply_into_next_command() {
        let mut idle = idle_fake_device(FakeDevice::new(10, None, 0));
        match idle.exec_sync("ver", Some(Duration::from_millis(20))) {
            Err(FleaTerminalError::Timeout { timeout }) => {
                assert_eq!(timeout, Duration::from_millis(20));
            }
            Err(e) => panic!("unexpected error: {e}"),
            Ok(r) => panic!("ver answered before its stall: {r:?}"),
        }
        let hostname = idle
            .exec_sync("hostname", Some(Duration::from_secs(1)))
            .expect("hostname after a timed-out command");
        assert_eq!(String::from_utf8_lossy(&hostname), "smoke");
    }

    #[test]
    fn exec_async_failed_write_does_not_corrupt_next_command() {
        let idle = idle_fake_device(FakeDevice::new(0, Some(3), 1));
        let mut idle = match idle.exec_async("read 1") {
            Err((idle, FleaTerminalError::Io(e))) => {
                assert_eq!(e.kind(), ErrorKind::TimedOut);
                idle
            }
            Err((_, e)) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("exec_async() succeeded despite a stalled write"),
        };
        let hostname = idle
            .exec_sync("hostname", Some(Duration::from_secs(1)))
            .expect("hostname after a failed exec_async");
        assert_eq!(String::from_utf8_lossy(&hostname), "smoke");
    }

    #[test]
    fn exec_async_failed_resync_does_not_corrupt_next_command() {
        // The stall also swallows the next call's resync, so "rea" stays on the line.
        let idle = idle_fake_device(FakeDevice::new(0, Some(3), 2));
        let mut idle = match idle.exec_async("read 1") {
            Err((idle, FleaTerminalError::Io(e))) => {
                assert_eq!(e.kind(), ErrorKind::TimedOut);
                idle
            }
            Err((_, e)) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("exec_async() succeeded despite a stalled write"),
        };
        match idle.exec_sync("hostname", Some(Duration::from_secs(1))) {
            Err(FleaTerminalError::Io(e)) => assert_eq!(e.kind(), ErrorKind::TimedOut),
            Err(e) => panic!("unexpected error: {e}"),
            Ok(r) => panic!("command ran despite a failed resync: {r:?}"),
        }
        let hostname = idle
            .exec_sync("hostname", Some(Duration::from_secs(1)))
            .expect("hostname once the resync gets through");
        assert_eq!(String::from_utf8_lossy(&hostname), "smoke");
    }

    #[test]
    fn exec_sync_failed_resync_does_not_leak_late_reply_into_next_command() {
        // "ver\n" gets through, the late reply times the caller out, and the
        // next call's resync is swallowed by a stalled write.
        let mut idle = idle_fake_device(FakeDevice::new(10, Some(4), 1));
        match idle.exec_sync("ver", Some(Duration::from_millis(20))) {
            Err(FleaTerminalError::Timeout { timeout }) => {
                assert_eq!(timeout, Duration::from_millis(20));
            }
            Err(e) => panic!("unexpected error: {e}"),
            Ok(r) => panic!("ver answered before its stall: {r:?}"),
        }
        match idle.exec_sync("hostname", Some(Duration::from_secs(1))) {
            Err(FleaTerminalError::Io(e)) => assert_eq!(e.kind(), ErrorKind::TimedOut),
            Err(e) => panic!("unexpected error: {e}"),
            Ok(r) => panic!("command ran despite a failed resync: {r:?}"),
        }
        let hostname = idle
            .exec_sync("hostname", Some(Duration::from_secs(1)))
            .expect("hostname once the resync gets through");
        assert_eq!(String::from_utf8_lossy(&hostname), "smoke");
    }

    #[test]
    fn exec_sync_timeout_stays_bounded_by_callers_timeout() {
        // A device that accepts writes but never answers: every call must
        // give up after the caller's timeout, including the pending resync.
        let mut idle = IdleFleaTerminal::new(StatelessFleaTerminal {
            serial: Box::new(SilentPort),
        });
        let timeout = Duration::from_millis(100);
        for call in 0..2 {
            let start = Instant::now();
            match idle.exec_sync("ver", Some(timeout)) {
                Err(FleaTerminalError::Timeout { timeout: t }) => assert_eq!(t, timeout),
                Err(e) => panic!("call {call}: unexpected error: {e}"),
                Ok(r) => panic!("call {call}: silent device answered: {r:?}"),
            }
            assert!(
                start.elapsed() < Duration::from_secs(1),
                "call {call} took {:?} with a {timeout:?} timeout",
                start.elapsed()
            );
        }
    }

    #[test]
    fn exec_sync_recovers_after_timeout_with_short_caller_timeout() {
        // Port-like timing: a read with nothing to deliver blocks 70ms, and the
        // reply's output and prompt come in separate reads. The resync's flush
        // alone takes one 70ms read, longer than the caller's 50ms timeout.
        let mut device = FakeDevice::new(2, None, 0);
        device.empty_read = Duration::from_millis(70);
        device.line_reads = true;
        let mut idle = idle_fake_device(device);
        let timeout = Duration::from_millis(50);
        match idle.exec_sync("ver", Some(timeout)) {
            Err(FleaTerminalError::Timeout { timeout: t }) => assert_eq!(t, timeout),
            Err(e) => panic!("unexpected error: {e}"),
            Ok(r) => panic!("ver answered before its stall: {r:?}"),
        }
        // The device answers again; within a few calls the terminal must be
        // usable, and stay usable, under the same short timeout.
        let recovered = (0..5).any(|_| idle.exec_sync("hostname", Some(timeout)).is_ok());
        assert!(
            recovered,
            "terminal never recovered under a {timeout:?} timeout"
        );
        let hostname = idle
            .exec_sync("hostname", Some(timeout))
            .expect("hostname right after recovery");
        assert_eq!(String::from_utf8_lossy(&hostname), "smoke");
    }

    #[test]
    fn flush_bytes_to_read_error_returns_error() {
        let mut device = FakeDevice::new(0, None, 0);
        device.bytes_to_read_fails = true;
        let stateless = StatelessFleaTerminal {
            serial: Box::new(device),
        };
        match IdleFleaTerminal::try_from(stateless) {
            Err((_, FleaTerminalError::SerialPort(_))) => {}
            Err((_, e)) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("connecting succeeded despite a bytes_to_read() error"),
        }
    }

    #[test]
    fn interrupted_read_in_try_get_result_keeps_polling() {
        let mut device = FakeDevice::new(0, None, 0);
        device.interrupted_reads = 1;
        let mut busy = match idle_fake_device(device).exec_async("hostname") {
            Ok(busy) => busy,
            Err((_, e)) => panic!("unexpected error: {e}"),
        };
        let hostname = loop {
            match busy.try_get_result() {
                Ok(TerminalStatus::Pending(b)) => busy = b,
                Ok(TerminalStatus::Done(data, _)) => break data,
                Err(ConnectionLostError) => panic!("an interrupted read lost the connection"),
            }
        };
        assert_eq!(String::from_utf8_lossy(&hostname), "smoke");
    }

    #[test]
    fn interrupted_read_in_cancel_still_returns_idle_terminal() {
        let mut device = FakeDevice::new(0, None, 0);
        device.interrupted_reads = 1;
        let busy = match idle_fake_device(device).exec_async("hostname") {
            Ok(busy) => busy,
            Err((_, e)) => panic!("unexpected error: {e}"),
        };
        let mut idle = busy.cancel().expect("cancel() despite an interrupted read");
        let hostname = idle
            .exec_sync("hostname", Some(Duration::from_secs(1)))
            .expect("hostname after cancel()");
        assert_eq!(String::from_utf8_lossy(&hostname), "smoke");
    }

    #[test]
    fn interrupted_read_in_flush_still_connects() {
        let mut device = FakeDevice::new(0, None, 0);
        device.interrupted_reads = 1;
        let stateless = StatelessFleaTerminal {
            serial: Box::new(device),
        };
        let mut idle = match IdleFleaTerminal::try_from(stateless) {
            Ok(idle) => idle,
            Err((_, e)) => panic!("connecting failed on an interrupted read: {e}"),
        };
        let hostname = idle
            .exec_sync("hostname", Some(Duration::from_secs(1)))
            .expect("hostname after connecting");
        assert_eq!(String::from_utf8_lossy(&hostname), "smoke");
    }

    #[test]
    fn flash_upgrade_after_failed_write_resyncs_and_detects_split_done() {
        // A stalled write leaves "rea" on the device's line; without a resync
        // `upgrade` would arrive as "reaupgrade" and never start. Reads of 4
        // bytes split "paste HEX upgrade file now" and "paste done!".
        let mut device = FakeDevice::new(0, Some(3), 1);
        device.max_read = 4;
        let idle = idle_fake_device(device);
        let mut idle = match idle.exec_async("read 1") {
            Err((idle, FleaTerminalError::Io(_))) => idle,
            Err((_, e)) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("exec_async() succeeded despite a stalled write"),
        };
        let records = [":10000000DEADBEEF", ":10001000CAFEBABE", ":00000001FF"];
        let mut progress = Vec::new();
        idle.flash_upgrade(&records, |p| progress.push((p.sent, p.total)))
            .expect("flash_upgrade");
        assert_eq!(progress, [(1, 3), (2, 3), (3, 3)]);
    }

    /// Flashing tools connect at the terminal layer, below `IdleFleaScope`'s
    /// firmware version check, so they can upgrade devices whose firmware the
    /// scope API no longer supports. Fails if a version check moves down here.
    #[test]
    fn flash_upgrade_works_on_unsupported_firmware_versions() {
        for ver in [
            "2.27g (checksum 0x7c8e)",
            "3.1.0 (checksum 0x2e47)",
            "9.0.0",
        ] {
            let mut device = FakeDevice::new(0, None, 0);
            device.ver_reply = ver;
            let stateless = StatelessFleaTerminal {
                serial: Box::new(device),
            };
            let mut idle = match IdleFleaTerminal::try_from(stateless) {
                Ok(idle) => idle,
                Err((_, e)) => panic!("{ver}: connecting failed: {e}"),
            };
            idle.flash_upgrade(&[":10000000DEADBEEF", ":00000001FF"], |_| {})
                .unwrap_or_else(|e| panic!("{ver}: flash_upgrade failed: {e}"));
        }
    }

    #[test]
    fn flash_upgrade_reports_device_failure() {
        // Reads of 4 bytes: "upgrade failed\r\n" ends a read exactly, so the
        // trailing "> " is left over for the next command unless it resyncs.
        let mut device = FakeDevice::new(0, None, 0);
        device.max_read = 4;
        let mut idle = idle_fake_device(device);
        match idle.flash_upgrade(&[":10000000DEADBEEF", ":BAD"], |_| {}) {
            Err(FlashUpgradeError::UpgradeFailed(output)) => {
                assert!(output.contains("upgrade failed"), "output: {output:?}");
            }
            Err(e) => panic!("unexpected error: {e}"),
            Ok(()) => panic!("flash_upgrade() succeeded despite `upgrade failed`"),
        }
        // The device is back at its prompt; the terminal must be usable again.
        let hostname = idle
            .exec_sync("hostname", Some(Duration::from_secs(1)))
            .expect("hostname after a failed upgrade");
        assert_eq!(String::from_utf8_lossy(&hostname), "smoke");
    }
}
