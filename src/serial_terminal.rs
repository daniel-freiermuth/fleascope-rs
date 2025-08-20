use serialport::SerialPort;
use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

const PROMPT: &[u8] = b"> ";

#[derive(Debug)]
pub struct FleaPreTerminal {
    serial: Box<dyn SerialPort>,
}

pub struct IdleFleaTerminal {
    inner: FleaPreTerminal,
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

impl FleaPreTerminal {
    /// Create a new FleaTerminal instance
    pub fn new(port: &str) -> Result<Self, FleaTerminalError> {
        #[cfg(feature = "puffin")]
        puffin::profile_function!();

        let serial = serialport::new(port, 9600)
            .timeout(Duration::from_millis(70))
            .open()?;

        let mut terminal = Self { serial };

        terminal.flush()?;
        Ok(terminal)
    }

    /// Initialize the terminal connection
    pub fn initialize(mut self) -> Result<IdleFleaTerminal, (Self, FleaTerminalError)> {
        #[cfg(feature = "puffin")]
        puffin::profile_function!();

        log::debug!("Connected to FleaScope. Sending CTRL-C to reset.");
        match self.send_ctrl_c() {
            Ok(_) => {}
            Err(e) => return Err((self, e)),
        };

        log::debug!("Turning on prompt");
        if let Err(e) = self.exec_sync("prompt on", Some(Duration::from_secs(1))) {
            return Err((self, e));
        };

        if let Err(e) = self.flush() {
            return Err((self, e));
        };
        Ok(IdleFleaTerminal { inner: self })
    }

    /// Flush the serial buffer
    fn flush(&mut self) -> Result<(), FleaTerminalError> {
        self.serial.clear(serialport::ClearBuffer::All)?;
        Ok(())
    }

    fn read_chunk(&mut self, response: &mut Vec<u8>) -> Result<bool, ConnectionLostError> {
        let mut read_buffer = [0u8; 1024]; // Read in chunks
        match self.serial.read(&mut read_buffer) {
            Ok(bytes_read) if bytes_read > 0 => {
                #[cfg(feature = "puffin")]
                puffin::profile_scope!("process_chunk_data", format!("{}", bytes_read));

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
                panic!("Serial read error: {}", e);
            }
        }
    }

    fn exec_sync(
        &mut self,
        command: &str,
        timeout: Option<Duration>,
    ) -> Result<Vec<u8>, FleaTerminalError> {
        #[cfg(feature = "puffin")]
        puffin::profile_function!();

        {
            #[cfg(feature = "puffin")]
            puffin::profile_scope!("serial_write_command");
            // Send command
            let command_with_newline = format!("{}\n", command);
            self.serial.write_all(command_with_newline.as_bytes())?;
        }

        // Read response until prompt
        #[cfg(feature = "puffin")]
        puffin::profile_scope!("serial_read_response");

        let mut response = Vec::new();
        let now = Instant::now();

        loop {
            #[cfg(feature = "puffin")]
            puffin::profile_scope!("serial_read_chunk");
            match self.read_chunk(&mut response) {
                Ok(true) => break,
                Ok(false) => {}
                Err(ConnectionLostError) => return Err(FleaTerminalError::ConnectionLost),
            };
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
        #[cfg(feature = "puffin")]
        puffin::profile_function!();

        let command_with_newline = format!("{}\n", command);
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
        #[cfg(feature = "puffin")]
        puffin::profile_function!();

        self.inner
            .exec_sync(command, timeout)
            .expect("Failed to execute command")
    }
}
pub struct BusyFleaTerminal {
    inner: FleaPreTerminal,
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
                Err(e) => panic!("Serial read error: {}", e),
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
        #[cfg(feature = "puffin")]
        puffin::profile_function!();

        // Remove the prompt from the end and convert to string
        let response_without_prompt = &self.response[..self.response.len() - PROMPT.len()];
        let response_str = response_without_prompt.to_vec();

        (response_str, IdleFleaTerminal { inner: self.inner })
    }

    pub fn is_ready(
        mut self,
    ) -> Result<Result<(Vec<u8>, IdleFleaTerminal), BusyFleaTerminal>, ConnectionLostError> {
        #[cfg(feature = "puffin")]
        puffin::profile_function!();

        #[cfg(feature = "puffin")]
        puffin::profile_scope!("serial_read_chunk");

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
