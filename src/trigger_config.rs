#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BitState {
    High,
    Low,
    DontCare,
}

#[derive(Debug, Clone, Copy)]
pub enum DigitalTriggerBehavior {
    Auto,
    While,
    Start,
    Stop,
}

impl DigitalTriggerBehavior {
    pub fn as_str(&self) -> &'static str {
        match self {
            DigitalTriggerBehavior::Auto => "~",
            DigitalTriggerBehavior::While => "",
            DigitalTriggerBehavior::Start => "+",
            DigitalTriggerBehavior::Stop => "-",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum AnalogTriggerBehavior {
    Auto,
    Level,
    Rising,
    Falling,
}

impl AnalogTriggerBehavior {
    pub fn as_str(&self) -> &'static str {
        match self {
            AnalogTriggerBehavior::Auto => "~",
            AnalogTriggerBehavior::Level => "",
            AnalogTriggerBehavior::Rising => "+",
            AnalogTriggerBehavior::Falling => "-",
        }
    }
}

#[derive(Debug)]
pub struct BitTriggerBuilder {
    bit_states: [BitState; 9],
}

impl BitTriggerBuilder {
    pub fn new() -> Self {
        Self {
            bit_states: [BitState::DontCare; 9],
        }
    }

    pub fn set_bit(mut self, bit: usize, state: BitState) -> Self {
        if bit >= self.bit_states.len() {
            panic!("Bit index {} out of range, must be between 0 and {}", bit, self.bit_states.len() - 1);
        }
        self.bit_states[bit] = state;
        self
    }

    pub fn bit0(self, state: BitState) -> Self {
        self.set_bit(0, state)
    }

    pub fn bit1(self, state: BitState) -> Self {
        self.set_bit(1, state)
    }

    pub fn bit2(self, state: BitState) -> Self {
        self.set_bit(2, state)
    }

    pub fn bit3(self, state: BitState) -> Self {
        self.set_bit(3, state)
    }

    pub fn bit4(self, state: BitState) -> Self {
        self.set_bit(4, state)
    }

    pub fn bit5(self, state: BitState) -> Self {
        self.set_bit(5, state)
    }

    pub fn bit6(self, state: BitState) -> Self {
        self.set_bit(6, state)
    }

    pub fn bit7(self, state: BitState) -> Self {
        self.set_bit(7, state)
    }

    pub fn bit8(self, state: BitState) -> Self {
        self.set_bit(8, state)
    }

    pub fn is_matching(self) -> DigitalTrigger {
        DigitalTrigger::new(self.bit_states, DigitalTriggerBehavior::While)
    }

    pub fn starts_matching(self) -> DigitalTrigger {
        DigitalTrigger::new(self.bit_states, DigitalTriggerBehavior::Start)
    }

    pub fn stops_matching(self) -> DigitalTrigger {
        DigitalTrigger::new(self.bit_states, DigitalTriggerBehavior::Stop)
    }

    /// Same as is_matching, but will also trigger when the bits did not match within 100ms.
    pub fn auto(self) -> DigitalTrigger {
        DigitalTrigger::new(self.bit_states, DigitalTriggerBehavior::Auto)
    }
}

impl Default for BitTriggerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct DigitalTrigger {
    bit_states: [BitState; 9],
    behavior: DigitalTriggerBehavior,
}

impl DigitalTrigger {
    pub fn new(bit_states: [BitState; 9], behavior: DigitalTriggerBehavior) -> Self {
        Self { bit_states, behavior }
    }

    pub fn start_capturing_when() -> BitTriggerBuilder {
        BitTriggerBuilder::new()
    }

    pub fn into_trigger_fields(&self) -> String {
        let mut relevant_bits = 0u32;
        for (i, state) in self.bit_states.iter().enumerate() {
            if *state != BitState::DontCare {
                relevant_bits |= 1 << i;
            }
        }

        let mut active_bits = 0u32;
        for (i, state) in self.bit_states.iter().enumerate() {
            if *state == BitState::High {
                active_bits |= 1 << i;
            }
        }

        let trigger_behavior_flag = self.behavior.as_str();
        format!("{}0x{:02x} 0x{:02x}", trigger_behavior_flag, active_bits, relevant_bits)
    }
}

#[derive(Debug)]
pub struct AnalogTriggerBuilder;

impl AnalogTriggerBuilder {
    pub fn new() -> Self {
        Self
    }

    pub fn rising_edge(&self, volts: f64) -> AnalogTrigger {
        AnalogTrigger::new(volts, AnalogTriggerBehavior::Rising)
    }

    pub fn falling_edge(&self, volts: f64) -> AnalogTrigger {
        AnalogTrigger::new(volts, AnalogTriggerBehavior::Falling)
    }

    pub fn level(&self, volts: f64) -> AnalogTrigger {
        AnalogTrigger::new(volts, AnalogTriggerBehavior::Level)
    }

    /// Same as level, but will also trigger when the voltage did not match within 100ms.
    pub fn auto(&self, volts: f64) -> AnalogTrigger {
        AnalogTrigger::new(volts, AnalogTriggerBehavior::Auto)
    }
}

impl Default for AnalogTriggerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct AnalogTrigger {
    level: f64,
    behavior: AnalogTriggerBehavior,
}

impl AnalogTrigger {
    pub fn new(level: f64, behavior: AnalogTriggerBehavior) -> Self {
        Self { level, behavior }
    }

    pub fn start_capturing_when() -> AnalogTriggerBuilder {
        AnalogTriggerBuilder::new()
    }

    pub fn into_trigger_fields<F>(&self, voltage_to_raw: F) -> Result<String, String>
    where
        F: Fn(f64) -> f64,
    {
        let trigger_behavior_flag = self.behavior.as_str();
        let raw_level = (voltage_to_raw(self.level) / 4.0 + 0.5) as i32;
        
        if raw_level < -1023 || raw_level > 1023 {
            return Err(format!(
                "Voltage {} out of range, must be between -1023 and 1023 raw units",
                self.level
            ));
        }

        Ok(format!("{}{} 0", trigger_behavior_flag, raw_level))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_digital_trigger_builder() {
        let trigger = DigitalTrigger::start_capturing_when()
            .bit0(BitState::High)
            .bit1(BitState::Low)
            .is_matching();
        
        let trigger_fields = trigger.into_trigger_fields();
        assert_eq!(trigger_fields, "0x01 0x03");
    }

    #[test]
    fn test_analog_trigger() {
        let trigger = AnalogTrigger::start_capturing_when()
            .rising_edge(1.5);
        
        let voltage_to_raw = |v: f64| v * 100.0; // Example conversion
        let trigger_fields = trigger.into_trigger_fields(voltage_to_raw).unwrap();
        assert_eq!(trigger_fields, "+38 0");
    }

    #[test]
    fn test_analog_trigger_out_of_range() {
        let trigger = AnalogTrigger::start_capturing_when()
            .level(100.0);
        
        let voltage_to_raw = |v: f64| v * 100.0; // This will create a value out of range
        let result = trigger.into_trigger_fields(voltage_to_raw);
        assert!(result.is_err());
    }

    #[test]
    #[should_panic(expected = "Bit index 9 out of range")]
    fn test_digital_trigger_builder_panic_on_invalid_bit() {
        DigitalTrigger::start_capturing_when()
            .set_bit(9, BitState::High); // This should panic since valid range is 0-8
    }
}
