use std::io::{Read, Write};
use std::time::Duration;
use serialport::{SerialPort};

#[derive(Debug)]
pub struct FleaTerminal {
    serial: Box<dyn SerialPort>,
    port: String,
    prompt: String,
    initialized: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum FleaTerminalError {
    #[error("Serial port error: {0}")]
    SerialPort(#[from] serialport::Error),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Timeout error: Expected prompt '{expected}' but got '{actual}'. Likely due to a timeout.")]
    Timeout { expected: String, actual: String },
    
    #[error("Terminal not initialized. Call initialize() first.")]
    NotInitialized,
    
    #[error("UTF-8 conversion error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}

impl FleaTerminal {
    /// Create a new FleaTerminal instance
    pub fn new(port: &str) -> Result<Self, FleaTerminalError> {
        let serial = serialport::new(port, 9600)
            .timeout(Duration::from_millis(1000))
            .open()?;
        
        let mut terminal = Self {
            serial,
            port: port.to_string(),
            prompt: "> ".to_string(),
            initialized: false,
        };
        
        terminal.flush()?;
        Ok(terminal)
    }
    
    /// Initialize the terminal connection
    pub fn initialize(&mut self) -> Result<(), FleaTerminalError> {
        log::debug!("Connected to FleaScope. Sending CTRL-C to reset.");
        self.send_ctrl_c()?;
        
        log::debug!("Turning on prompt");
        self.exec_internal("prompt on", Some(Duration::from_secs(1)))?;
        
        self.initialized = true;
        self.flush()?;
        Ok(())
    }
    
    /// Flush the serial buffer
    fn flush(&mut self) -> Result<(), FleaTerminalError> {
        self.serial.clear(serialport::ClearBuffer::All)?;
        Ok(())
    }
    
    /// Execute a command (public interface)
    pub fn exec(&mut self, command: &str, timeout: Option<Duration>) -> Result<String, FleaTerminalError> {
        if !self.initialized {
            return Err(FleaTerminalError::NotInitialized);
        }
        self.exec_internal(command, timeout)
    }
    
    /// Execute a command (internal implementation)
    fn exec_internal(&mut self, command: &str, timeout: Option<Duration>) -> Result<String, FleaTerminalError> {
        // Send command
        let command_with_newline = format!("{}\n", command);
        self.serial.write_all(command_with_newline.as_bytes())?;
        
        // Set timeout
        if let Some(timeout) = timeout {
            self.serial.set_timeout(timeout)?;
        }
        
        // Read response until prompt
        let mut response = Vec::new();
        let prompt_bytes = self.prompt.as_bytes();
        let mut window = Vec::new();
        
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
                    // Handle timeout
                    let _response_str = String::from_utf8_lossy(&response);
                    let actual_ending = if response.len() >= 2 {
                        String::from_utf8_lossy(&response[response.len()-2..]).to_string()
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
    
    /// Check if the terminal is initialized
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flea_terminal_creation() {
        // This test will fail if no serial port is available, which is expected in CI
        // In a real test environment, you would use a mock serial port
        let result = FleaTerminal::new("/dev/null");
        // We can't easily test this without a real or mock serial port
        // but we can at least verify the error handling works
        match result {
            Ok(_) => {
                // If /dev/null works as a serial port on this system, that's fine
            }
            Err(FleaTerminalError::SerialPort(_)) => {
                // Expected when /dev/null is not a valid serial port
            }
            Err(e) => {
                panic!("Unexpected error type: {:?}", e);
            }
        }
    }
    
    #[test]
    fn test_not_initialized_error() {
        // Test that we get proper error when trying to exec before initialize
        if let Ok(mut terminal) = FleaTerminal::new("/dev/null") {
            let result = terminal.exec("test command", None);
            assert!(matches!(result, Err(FleaTerminalError::NotInitialized)));
        }
    }
}
