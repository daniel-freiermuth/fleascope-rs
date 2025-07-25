use serialport::SerialPort;
use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct FleaPreTerminal {
    serial: Box<dyn SerialPort>,
    prompt: String,
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

    #[error(
        "Timeout error: Expected prompt '{expected}' but got '{actual}'. Likely due to a timeout."
    )]
    Timeout { expected: String, actual: String },

    #[error("UTF-8 conversion error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),

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

        let mut terminal = Self {
            serial,
            prompt: "> ".to_string(),
        };

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
        if let Err(e) = self.exec("prompt on", Some(Duration::from_secs(1))) {
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

    fn exec(
        &mut self,
        command: &str,
        timeout: Option<Duration>,
    ) -> Result<String, FleaTerminalError> {
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
        let prompt_bytes = self.prompt.as_bytes();
        let mut read_buffer = [0u8; 1024]; // Read in chunks instead of byte-by-byte
        let now = Instant::now();

        loop {
            #[cfg(feature = "puffin")]
            puffin::profile_scope!("serial_read_chunk");
            
            match self.serial.read(&mut read_buffer) {
                Ok(bytes_read) if bytes_read > 0 => {
                    #[cfg(feature = "puffin")]
                    puffin::profile_scope!("process_chunk_data");
                    
                    response.extend_from_slice(&read_buffer[..bytes_read]);
                    
                    // Check if we have the prompt at the end
                    if response.len() >= prompt_bytes.len() {
                        let potential_prompt = &response[response.len() - prompt_bytes.len()..];
                        if potential_prompt == prompt_bytes {
                            break;
                        }
                    }
                }
                Ok(_) => {
                    // No data available, check timeout
                    if let Some(t) = timeout {
                        if now.elapsed() >= t {
                            let _response_str = String::from_utf8_lossy(&response);
                            let actual_ending = if response.len() >= 2 {
                                String::from_utf8_lossy(&response[response.len() - 2..]).to_string()
                            } else {
                                String::from_utf8_lossy(&response).to_string()
                            };

                            return Err(FleaTerminalError::Timeout {
                                expected: self.prompt.clone(),
                                actual: actual_ending,
                            });
                        }
                    }
                }
                Err(e) if e.kind() == ErrorKind::TimedOut => {
                    // Timeout on read, check overall timeout
                    if let Some(t) = timeout {
                        if now.elapsed() >= t {
                            let _response_str = String::from_utf8_lossy(&response);
                            let actual_ending = if response.len() >= 2 {
                                String::from_utf8_lossy(&response[response.len() - 2..]).to_string()
                            } else {
                                String::from_utf8_lossy(&response).to_string()
                            };

                            return Err(FleaTerminalError::Timeout {
                                expected: self.prompt.clone(),
                                actual: actual_ending,
                            });
                        }
                    }
                    // Continue reading if we haven't hit overall timeout
                }
                Err(e) => {
                    return Err(FleaTerminalError::Io(e));
                }
            }
        }

        // Remove the prompt from the end and convert to string
        let response_without_prompt = &response[..response.len() - prompt_bytes.len()];
        let response_str = String::from_utf8(response_without_prompt.to_vec())?;

        Ok(response_str.trim().to_string())
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

        let prompt_bytes = self.inner.prompt.as_bytes().to_vec();

        BusyFleaTerminal {
            inner: self.inner,
            response: Vec::new(),
            prompt_bytes,
            start: Instant::now(),
            done: false,
        }
    }
    pub fn exec_sync(&mut self, command: &str, timeout: Option<Duration>) -> String {
        #[cfg(feature = "puffin")]
        puffin::profile_function!();
        
        self.inner
            .exec(command, timeout)
            .expect("Failed to execute command")
    }
}
pub struct BusyFleaTerminal {
    inner: FleaPreTerminal,
    response: Vec<u8>,
    prompt_bytes: Vec<u8>,
    start: Instant,
    done: bool,
}

impl BusyFleaTerminal {
    pub fn wait(mut self) -> (Result<String, ConnectionLostError>, IdleFleaTerminal) {
        #[cfg(feature = "puffin")]
        puffin::profile_function!();
        
        loop {
            match self.is_ready() {
                Ok(b) => {
                    if b {
                        let (str, idlescope) = self.generate_result();
                        return (Ok(str), idlescope);
                    }
                }
                Err(e) => {
                    return (Err(e), IdleFleaTerminal { inner: self.inner });
                }
            }
        }
    }

    pub fn cancel(&mut self) {
        self.inner.send_ctrl_c().expect("Failed to send CTRL-C");
    }

    fn generate_result(self) -> (String, IdleFleaTerminal) {
        #[cfg(feature = "puffin")]
        puffin::profile_function!();
        
        // Remove the prompt from the end and convert to string
        let response_without_prompt =
            &self.response[..self.response.len() - self.prompt_bytes.len()];
        let response_str = String::from_utf8(response_without_prompt.to_vec())
            .expect("Failed to convert response to string");

        (
            response_str.trim().to_string(),
            IdleFleaTerminal { inner: self.inner },
        )
    }

    pub fn wait_timeout(
        mut self,
        timeout: Duration,
    ) -> Result<(String, IdleFleaTerminal), (BusyFleaTerminal, FleaTerminalError)> {
        loop {
            match self.is_ready() {
                Ok(done) => {
                    if done {
                        return Ok(self.generate_result());
                    }
                }
                Err(_e) => {
                    return Err((self, FleaTerminalError::ConnectionLost));
                }
            }

            if self.start.elapsed() >= timeout {
                let _response_str = String::from_utf8_lossy(&self.response);
                let actual_ending = if self.response.len() >= 2 {
                    String::from_utf8_lossy(&self.response[self.response.len() - 2..]).to_string()
                } else {
                    String::from_utf8_lossy(&self.response).to_string()
                };

                let expected = self.inner.prompt.clone();
                return Err((
                    self,
                    FleaTerminalError::Timeout {
                        expected,
                        actual: actual_ending,
                    },
                ));
            }
        }
    }

    pub fn is_ready(&mut self) -> Result<bool, ConnectionLostError> {
        #[cfg(feature = "puffin")]
        puffin::profile_function!();
        
        if self.done {
            return Ok(true);
        }
        
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

        let mut read_buffer = [0u8; 1024]; // Read in chunks
        match self.inner.serial.read(&mut read_buffer) {
            Ok(bytes_read) if bytes_read > 0 => {
                #[cfg(feature = "puffin")]
                puffin::profile_scope!("process_chunk_data");

                self.response.extend_from_slice(&read_buffer[..bytes_read]);
                
                // Check if we have the prompt at the end
                if self.response.len() >= self.prompt_bytes.len() {
                    let potential_prompt = &self.response[self.response.len() - self.prompt_bytes.len()..];
                    if potential_prompt == self.prompt_bytes {
                        self.done = true;
                    }
                }
            }
            Ok(_) => {
                // No data available right now, but no error
            }
            Err(e) if e.kind() == ErrorKind::TimedOut => {
                // Timeout is expected in non-blocking reads
            }
            Err(e) if e.kind() == ErrorKind::BrokenPipe => {
                return Err(ConnectionLostError);
            }
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => {
                return Err(ConnectionLostError);
            }
            Err(e) => {
                tracing::info!("Serial read error (kind: {:?})...{e}", e.kind());
                panic!("Serial read error: {}", e);
            }
        }
        
        Ok(self.done)
    }
}
