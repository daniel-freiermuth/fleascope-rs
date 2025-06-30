use serialport::SerialPort;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct FleaPreTerminal {
    serial: Box<dyn SerialPort>,
    prompt: String,
}

pub struct IdleFleaTerminal {
    inner: FleaPreTerminal,
}

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
}

impl FleaPreTerminal {
    /// Create a new FleaTerminal instance
    pub fn new(port: &str) -> Result<Self, FleaTerminalError> {
        let serial = serialport::new(port, 9600)
            .timeout(Duration::from_millis(10))
            .open()?;

        let mut terminal = Self {
            serial: serial,
            prompt: "> ".to_string(),
        };

        terminal.flush()?;
        Ok(terminal)
    }

    /// Initialize the terminal connection
    pub fn initialize(mut self) -> Result<IdleFleaTerminal, (Self, FleaTerminalError)> {
        log::debug!("Connected to FleaScope. Sending CTRL-C to reset.");
        match self.send_ctrl_c() {
            Ok(_) => {}
            Err(e) => return Err((self, e)),
        };

        log::debug!("Turning on prompt");
        match self.exec("prompt on", Some(Duration::from_secs(1))) {
            Err(e) => return Err((self, e)),
            Ok(_) => {},
        };

        match self.flush() {
            Err(e) => return Err((self, e)),
            Ok(_) => {},
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
        {
            // Send command
            let command_with_newline = format!("{}\n", command);
            self.serial.write_all(command_with_newline.as_bytes())?;

        }

        // Read response until prompt
        let mut response = Vec::new();
        let prompt_bytes = self.prompt.as_bytes();
        let mut window = Vec::new();
        let now = Instant::now();

        loop {
            let mut byte = [0u8; 1];
            match self.serial.read_exact(&mut byte) {
                Ok(_) => {
                    response.push(byte[0]);
                    window.push(byte[0]);

                    // Keep window size equal to prompt length
                    if window.len() > prompt_bytes.len() {
                        window.remove(0);
                    }

                    // Check if we found the prompt
                    if window.len() == prompt_bytes.len() && window == prompt_bytes {
                        break;
                    }
                }
                Err(_e) => {
                    match timeout {
                        Some(t) if now.elapsed() >= t => {
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
                        _ => {}
                    }
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
    pub fn exec_async(
        mut self,
        command: &str,
    ) -> BusyFleaTerminal {
        let command_with_newline = format!("{}\n", command);
        self.inner.serial.write_all(command_with_newline.as_bytes()).expect("Failed to write command to serial port");

        let prompt_bytes = self.inner.prompt.as_bytes().to_vec();

        BusyFleaTerminal {
            inner: self.inner,
            response: Vec::new(),
            prompt_bytes,
            window: Vec::new(),
            start: Instant::now(),
            done: false,
        }
    }
    pub fn exec_sync(
        &mut self,
        command: &str,
        timeout: Option<Duration>,
    ) -> String {
        self.inner.exec(command, timeout).expect("Failed to execute command")
    }
}
pub struct BusyFleaTerminal {
    inner: FleaPreTerminal,
    response: Vec<u8>,
    prompt_bytes: Vec<u8>,
    window: Vec<u8>,
    start: Instant,
    done: bool,
}

impl BusyFleaTerminal {
    pub fn wait(mut self) -> (String, IdleFleaTerminal) {
        loop {
            if self.try_get() {
                return self.generate_result();
            }
        }
    }

    pub fn cancel(&mut self) -> () {
        self.inner.send_ctrl_c();
    }

    fn generate_result(self) -> (String, IdleFleaTerminal) {
        // Remove the prompt from the end and convert to string
        let response_without_prompt = &self.response[..self.response.len() - self.prompt_bytes.len()];
        let response_str = String::from_utf8(response_without_prompt.to_vec()).expect("Failed to convert response to string");

        (
            response_str.trim().to_string(),
            IdleFleaTerminal { inner: self.inner },
        )
    }

    pub fn wait_timeout(mut self, timeout: Duration) -> Result<(String, IdleFleaTerminal), (BusyFleaTerminal, FleaTerminalError)> {
        loop {
            if self.try_get() {
                return Ok(self.generate_result());
            }

            if self.start.elapsed() >= timeout {
                let _response_str = String::from_utf8_lossy(&self.response);
                let actual_ending = if self.response.len() >= 2 {
                    String::from_utf8_lossy(&self.response[self.response.len() - 2..]).to_string()
                } else {
                    String::from_utf8_lossy(&self.response).to_string()
                };

                let expected = self.inner.prompt.clone();
                return Err((self, FleaTerminalError::Timeout {
                    expected,
                    actual: actual_ending,
                }));
            }
        }
    }

    pub fn try_get(&mut self) -> bool {
        while !self.done {
            let mut byte = [0u8; 1];
            match self.inner.serial.read_exact(&mut byte) {
                Ok(_) => {
                    self.response.push(byte[0]);
                    self.window.push(byte[0]);

                    if self.window.len() > self.prompt_bytes.len() {
                        self.window.remove(0);
                    }

                    if self.window.len() == self.prompt_bytes.len() && self.window == self.prompt_bytes {
                        self.done = true;
                    }
                }
                Err(_e) => {
                    break;
                }
            }
        }
        self.done
    }
}
