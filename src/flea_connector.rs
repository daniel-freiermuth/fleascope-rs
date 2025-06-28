use std::thread;
use std::time::Duration;
use crate::serial_terminal::{FleaTerminal, FleaTerminalError};

#[derive(Debug, Clone)]
pub struct FleaDevice {
    pub name: String,
    pub port: String,
}

impl FleaDevice {
    pub fn new(name: String, port: String) -> Self {
        Self { name, port }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FleaConnectorError {
    #[error("Serial terminal error: {0}")]
    SerialTerminal(#[from] FleaTerminalError),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Port {port} is not the FleaScope device you're looking for")]
    InvalidPort { port: String },
    
    #[error("No FleaScope device {name} found. Please connect a FleaScope or specify the port manually")]
    DeviceNotFound { name: String },
    
    #[error("Device validation failed")]
    DeviceValidationFailed,
}

pub struct FleaConnector;

impl FleaConnector {
    /// Connect to a FleaScope device
    pub fn connect(
        name: Option<&str>,
        port: Option<&str>,
        _read_calibrations: bool,
    ) -> Result<FleaTerminal, FleaConnectorError> {
        let mut terminal = if let Some(port) = port {
            log::debug!("Connecting to FleaScope on port {}", port);
            Self::validate_port(name, port)?;
            FleaTerminal::new(port)?
        } else {
            let device_name = name.unwrap_or("FleaScope");
            Self::get_working_serial(device_name)?
        };
        
        terminal.initialize()?;
        Ok(terminal)
    }
    
    /// Validate that a given port corresponds to a FleaScope device
    fn validate_port(name: Option<&str>, port: &str) -> Result<(), FleaConnectorError> {
        // For now, we'll do a simplified validation
        // In a full implementation, you'd use udev to check device properties
        let devices = Self::get_available_devices(name)?;
        
        if !devices.iter().any(|d| d.port == port) {
            return Err(FleaConnectorError::InvalidPort {
                port: port.to_string(),
            });
        }
        
        Ok(())
    }
    
    /// Validate that a udev device is a FleaScope
    fn validate_device(name: Option<&str>, device: &udev::Device) -> bool {
        let valid_vendor_model_variants = [
            ["0403", "a660"],
            ["1b4f", "a660"],
            ["1b4f", "e66e"],
            ["04d8", "e66e"],
        ];
        
        // Check if device has required properties
        let vendor_id = match device.property_value("ID_VENDOR_ID") {
            Some(id) => id.to_string_lossy(),
            None => return false,
        };
        
        let model_id = match device.property_value("ID_MODEL_ID") {
            Some(id) => id.to_string_lossy(),
            None => return false,
        };
        
        let model = match device.property_value("ID_MODEL") {
            Some(model) => model.to_string_lossy(),
            None => return false,
        };
        
        // Check if vendor/model combination is valid
        let is_valid_variant = valid_vendor_model_variants
            .iter()
            .any(|[vid, mid]| vendor_id == *vid && model_id == *mid);
        
        if !is_valid_variant {
            return false;
        }
        
        // Check if name matches (if specified)
        if let Some(expected_name) = name {
            if model != expected_name {
                return false;
            }
        }
        
        true
    }
    
    /// Get all available FleaScope devices
    pub fn get_available_devices(name: Option<&str>) -> Result<Vec<FleaDevice>, FleaConnectorError> {
        let mut enumerator = udev::Enumerator::new()?;
        enumerator.match_subsystem("tty")?;
        
        let mut devices = Vec::new();
        
        for device in enumerator.scan_devices()? {
            if Self::validate_device(name, &device) {
                if let (Some(model), Some(device_node)) = (
                    device.property_value("ID_MODEL"),
                    device.devnode(),
                ) {
                    devices.push(FleaDevice::new(
                        model.to_string_lossy().to_string(),
                        device_node.to_string_lossy().to_string(),
                    ));
                }
            }
        }
        
        Ok(devices)
    }
    
    /// Get the port for a device with the given name
    fn get_device_port(name: &str) -> Result<String, FleaConnectorError> {
        log::debug!("Searching for FleaScope device with name {}", name);
        
        let devices = Self::get_available_devices(Some(name))?;
        
        devices
            .into_iter()
            .next()
            .map(|device| device.port)
            .ok_or_else(|| FleaConnectorError::DeviceNotFound {
                name: name.to_string(),
            })
    }
    
    /// Get a working serial connection, retrying if necessary
    fn get_working_serial(name: &str) -> Result<FleaTerminal, FleaConnectorError> {
        loop {
            let port_candidate = Self::get_device_port(name)?;
            let mut serial = FleaTerminal::new(&port_candidate)?;
            
            match serial.initialize() {
                Ok(_) => break Ok(serial),
                Err(FleaTerminalError::Timeout { .. }) => {
                    log::debug!("Timeout during initialization, sending reset and retrying");
                    let _ = serial.send_reset(); // Ignore errors here
                    thread::sleep(Duration::from_secs(2));
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_available_devices() {
        // This test will depend on what devices are actually connected
        // In a real test environment, you might want to mock the udev calls
        let result = FleaConnector::get_available_devices(None);
        
        match result {
            Ok(devices) => {
                // If we find devices, make sure they have valid names and ports
                for device in devices {
                    assert!(!device.name.is_empty());
                    assert!(!device.port.is_empty());
                    assert!(device.port.starts_with('/'));
                }
            }
            Err(FleaConnectorError::Io(_)) => {
                // Expected if udev is not available or accessible
            }
            Err(e) => {
                panic!("Unexpected error: {:?}", e);
            }
        }
    }
    
    #[test]
    fn test_device_validation() {
        // Test the validation logic with mock data would require more complex mocking
        // For now, we can at least test that the function doesn't panic
        // In a real implementation, you'd want to create mock udev::Device objects
    }
}
