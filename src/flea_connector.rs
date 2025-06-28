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
    
    #[error("Serial port error: {0}")]
    SerialPort(#[from] serialport::Error),
    
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
        let devices = Self::get_available_devices(name)?;
        
        if !devices.iter().any(|d| d.port == port) {
            return Err(FleaConnectorError::InvalidPort {
                port: port.to_string(),
            });
        }
        
        Ok(())
    }
    
    /// Validate that a serial port info represents a FleaScope device
    fn validate_device(name: Option<&str>, port_info: &serialport::SerialPortInfo) -> bool {
        // Valid vendor/product ID combinations for FleaScope devices
        let valid_vendor_product_variants = [
            (0x0403, 0xa660), // FTDI vendor, FleaScope product
            (0x1b4f, 0xa660), // SparkFun vendor, FleaScope product  
            (0x1b4f, 0xe66e), // SparkFun vendor, alternative product
            (0x04d8, 0xe66e), // Microchip vendor, alternative product
        ];
        
        // Only check USB devices
        let usb_info = match &port_info.port_type {
            serialport::SerialPortType::UsbPort(usb_info) => usb_info,
            _ => return false,
        };
        
        // Check if vendor/product combination is valid
        let is_valid_variant = valid_vendor_product_variants
            .iter()
            .any(|(vid, pid)| usb_info.vid == *vid && usb_info.pid == *pid);
        
        if !is_valid_variant {
            return false;
        }
        
        // Check if name matches (if specified)
        if let Some(expected_name) = name {
            if let Some(product_name) = &usb_info.product {
                if product_name != expected_name {
                    return false;
                }
            } else {
                return false;
            }
        }
        
        true
    }
    
    /// Get all available FleaScope devices
    pub fn get_available_devices(name: Option<&str>) -> Result<Vec<FleaDevice>, FleaConnectorError> {
        let ports = serialport::available_ports()?;
        
        let mut devices = Vec::new();
        
        for port_info in ports {
            if Self::validate_device(name, &port_info) {
                let device_name = if let serialport::SerialPortType::UsbPort(usb_info) = &port_info.port_type {
                    usb_info.product.clone()
                        .or_else(|| usb_info.manufacturer.clone())
                        .unwrap_or_else(|| "FleaScope".to_string())
                } else {
                    "FleaScope".to_string()
                };
                
                devices.push(FleaDevice::new(
                    device_name,
                    port_info.port_name,
                ));
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
        let result = FleaConnector::get_available_devices(None);
        
        match result {
            Ok(devices) => {
                // If we find devices, make sure they have valid names and ports
                for device in devices {
                    assert!(!device.name.is_empty());
                    assert!(!device.port.is_empty());
                    // Port should be a valid path (Unix) or COM port (Windows)
                    assert!(device.port.starts_with('/') || device.port.starts_with("COM"));
                }
            }
            Err(FleaConnectorError::SerialPort(_)) => {
                // Expected if serial port enumeration fails
            }
            Err(e) => {
                panic!("Unexpected error: {:?}", e);
            }
        }
    }
    
    #[test]
    fn test_device_validation_logic() {
        // Test the validation logic with some example data
        let valid_usb_info = serialport::UsbPortInfo {
            vid: 0x0403,
            pid: 0xa660,
            serial_number: Some("12345".to_string()),
            manufacturer: Some("FTDI".to_string()),
            product: Some("FleaScope".to_string()),
        };
        
        let valid_port_info = serialport::SerialPortInfo {
            port_name: "/dev/ttyUSB0".to_string(),
            port_type: serialport::SerialPortType::UsbPort(valid_usb_info),
        };
        
        assert!(FleaConnector::validate_device(None, &valid_port_info));
        assert!(FleaConnector::validate_device(Some("FleaScope"), &valid_port_info));
        assert!(!FleaConnector::validate_device(Some("OtherDevice"), &valid_port_info));
        
        // Test with invalid VID/PID
        let invalid_usb_info = serialport::UsbPortInfo {
            vid: 0x1234,
            pid: 0x5678,
            serial_number: None,
            manufacturer: None,
            product: None,
        };
        
        let invalid_port_info = serialport::SerialPortInfo {
            port_name: "/dev/ttyUSB1".to_string(),
            port_type: serialport::SerialPortType::UsbPort(invalid_usb_info),
        };
        
        assert!(!FleaConnector::validate_device(None, &invalid_port_info));
    }
}
