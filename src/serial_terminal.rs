use serialport::SerialPort;
use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

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

    fn read_chunk(&mut self, response: &mut Vec<u8>) -> Result<bool, ConnectionLostError> {
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
            Err(e) if e.kind() == ErrorKind::BrokenPipe => Err(ConnectionLostError),
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => Err(ConnectionLostError),
            Err(e) => {
                tracing::info!("Serial read error (kind: {:?})...{e}", e.kind());
                panic!("Serial read error: {e}");
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
                Err(ConnectionLostError) => return Err(FleaTerminalError::ConnectionLost),
            }
            if let Some(t) = timeout {
                if now.elapsed() >= t {
                    return Err(FleaTerminalError::Timeout { timeout: t });
                }
            }
        }

        // Remove the prompt from the end
        let response_without_prompt = &response[..response.len() - PROMPT.len()];

        // Trim leading/trailing ASCII whitespace (command echo, \r\n)
        let start = response_without_prompt
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .unwrap_or(response_without_prompt.len());
        let end = response_without_prompt
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(start, |i| i + 1);
        let trimmed = response_without_prompt[start..end].to_vec();

        Ok(trimmed)
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
}

impl IdleFleaTerminal {
    pub fn exec_async(
        mut self,
        command: &str,
    ) -> Result<BusyFleaTerminal, (Self, FleaTerminalError)> {
        profiling::scope!("IdleFleaTerminal::exec_async");

        let command_with_newline = format!("{command}\n");
        if let Err(e) = self.inner.serial.write_all(command_with_newline.as_bytes()) {
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

        self.inner.exec_sync(command, timeout)
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
    pub fn cancel(mut self) -> Result<IdleFleaTerminal, FleaTerminalError> {
        self.inner.send_ctrl_c()?;
        const CANCEL_TIMEOUT: Duration = Duration::from_secs(5);
        let cancel_start = Instant::now();
        const PROMPT_LEN: usize = PROMPT.len();
        const BUFFER_LEN: usize = 1024;
        let mut prompt_buffer = VecDeque::with_capacity(PROMPT_LEN);
        let mut read_buffer = [0u8; BUFFER_LEN];
        loop {
            match self.inner.serial.read(&mut read_buffer) {
                Ok(bytes_read) if bytes_read > 0 => {
                    prompt_window_extend(
                        &mut prompt_buffer,
                        &read_buffer[..bytes_read],
                        PROMPT_LEN,
                    );
                }
                Ok(_) => {} // No data available right now, but no error
                Err(e) if e.kind() == ErrorKind::TimedOut => {} // Timeout is expected in non-blocking reads
                Err(e) if e.kind() == ErrorKind::BrokenPipe => {
                    return Err(FleaTerminalError::ConnectionLost)
                }
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                    return Err(FleaTerminalError::ConnectionLost)
                }
                Err(e) => panic!("Serial read error: {e}"),
            }
            // Check if we have the prompt at the end
            if prompt_buffer.len() == PROMPT.len()
                && prompt_buffer.iter().copied().eq(PROMPT.iter().copied())
            {
                break;
            }
            if cancel_start.elapsed() >= CANCEL_TIMEOUT {
                return Err(FleaTerminalError::Timeout {
                    timeout: CANCEL_TIMEOUT,
                });
            }
        }
        self.inner.flush()?;
        Ok(IdleFleaTerminal { inner: self.inner })
    }

    fn into_result(self) -> (Vec<u8>, IdleFleaTerminal) {
        profiling::scope!("BusyFleaTerminal::into_result");

        // Remove the prompt from the end
        let response_without_prompt = &self.response[..self.response.len() - PROMPT.len()];

        // Trim leading/trailing ASCII whitespace (command echo, \r\n)
        let start = response_without_prompt
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .unwrap_or(response_without_prompt.len());
        let end = response_without_prompt
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(start, |i| i + 1);
        let trimmed = response_without_prompt[start..end].to_vec();

        (trimmed, IdleFleaTerminal { inner: self.inner })
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
            Err(ConnectionLostError) => Err(ConnectionLostError),
        }
    }
}

impl Read for BusyFleaTerminal {
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, std::io::Error> {
        profiling::scope!("BusyFleaTerminal::read");

        self.inner.serial.read(buffer)
    }
}

fn prompt_window_extend(buf: &mut VecDeque<u8>, new: &[u8], window_len: usize) {
    buf.extend(new);
    while buf.len() > window_len {
        buf.pop_front();
    }
}

#[cfg(test)]
mod tests {
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
        for &b in b"XY> " { prompt_window_extend(&mut buf, std::slice::from_ref(&b), PROMPT.len()); }
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
    /// `writable` keeps writes alive to model an unplug later in `cancel()`.
    struct UnpluggedPort {
        writable: bool,
        pending: Vec<u8>,
    }

    impl Read for UnpluggedPort {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pending.is_empty() {
                return Err(ErrorKind::BrokenPipe.into());
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
        });
        match busy.cancel() {
            Err(FleaTerminalError::SerialPort(_)) => {}
            Err(e) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("cancel() succeeded on an unplugged port"),
        }
    }

    fn idle_terminal(port: UnpluggedPort) -> IdleFleaTerminal {
        IdleFleaTerminal {
            inner: StatelessFleaTerminal {
                serial: Box::new(port),
            },
        }
    }

    #[test]
    fn exec_async_on_unplugged_port_returns_error() {
        let idle = idle_terminal(UnpluggedPort {
            writable: false,
            pending: Vec::new(),
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
        });
        match idle.exec_sync("ver", None) {
            Err(FleaTerminalError::ConnectionLost) => {}
            Err(e) => panic!("unexpected error: {e}"),
            Ok(_) => panic!("exec_sync() succeeded on an unplugged port"),
        }
    }
}
