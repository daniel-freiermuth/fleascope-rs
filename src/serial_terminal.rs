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
        #[cfg(feature = "cpu-profiling")]
        let _span = tracy_client::span!();

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
        #[cfg(feature = "cpu-profiling")]
        let _span = tracy_client::span!();
        match self.serial.read(&mut read_buffer) {
            Ok(bytes_read) if bytes_read > 0 => {
                #[cfg(feature = "cpu-profiling")]
                let _span = tracy_client::span!("process_chunk_data");

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
        #[cfg(feature = "cpu-profiling")]
        let _span = tracy_client::span!();

        {
            #[cfg(feature = "cpu-profiling")]
            let _span = tracy_client::span!("serial_write_command");
            // Send command
            let command_with_newline = format!("{command}\n");
            self.serial.write_all(command_with_newline.as_bytes())?;
        }

        // Read response until prompt
        #[cfg(feature = "cpu-profiling")]
        let _span = tracy_client::span!("serial_read_response");

        let mut response = Vec::new();
        let now = Instant::now();

        loop {
            #[cfg(feature = "cpu-profiling")]
            let _span = tracy_client::span!("serial_read_chunk");
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
}

impl IdleFleaTerminal {
    pub fn exec_async(mut self, command: &str) -> BusyFleaTerminal {
        #[cfg(feature = "cpu-profiling")]
        let _span = tracy_client::span!();

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
        #[cfg(feature = "cpu-profiling")]
        let _span = tracy_client::span!();

        self.inner
            .exec_sync(command, timeout)
            .expect("Failed to execute command")
    }
}
impl TryFrom<StatelessFleaTerminal> for IdleFleaTerminal {
    type Error = (StatelessFleaTerminal, FleaTerminalError);

    fn try_from(mut value: StatelessFleaTerminal) -> Result<Self, Self::Error> {
        #[cfg(feature = "cpu-profiling")]
        let _span = tracy_client::span!();

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
                Err(e) => panic!("Serial read error: {e}"),
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
        #[cfg(feature = "cpu-profiling")]
        let _span = tracy_client::span!();

        // Remove the prompt from the end and convert to string
        let response_without_prompt = &self.response[..self.response.len() - PROMPT.len()];
        let response_str = response_without_prompt.to_vec();

        (response_str, IdleFleaTerminal { inner: self.inner })
    }

    pub fn try_get_result(
        mut self,
    ) -> Result<Result<(Vec<u8>, IdleFleaTerminal), Self>, ConnectionLostError> {
        #[cfg(feature = "cpu-profiling")]
        let _span = tracy_client::span!();

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
        #[cfg(feature = "cpu-profiling")]
        let _span = tracy_client::span!();

        self.inner.serial.read(buffer)
    }
}
