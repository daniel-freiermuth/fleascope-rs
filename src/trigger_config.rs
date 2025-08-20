use crate::{flea_scope::CaptureConfigError, FleaProbe};

pub trait TriggerConfig {
    fn into_trigger_fields(self) -> StringifiedTriggerConfig;
}

pub struct StringifiedTriggerConfig {
    trigger_fields: String,
}

impl StringifiedTriggerConfig {
    pub fn into_string(self) -> String {
        self.trigger_fields
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BitState {
    High,
    Low,
    DontCare,
}

#[derive(Debug, Clone, Copy, PartialEq)]
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

#[derive(Debug, Clone, Copy, PartialEq)]
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
            panic!(
                "Bit index {} out of range, must be between 0 and {}",
                bit,
                self.bit_states.len() - 1
            );
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

#[derive(Debug, Clone)]
pub struct DigitalTrigger {
    pub bit_states: [BitState; 9],
    pub behavior: DigitalTriggerBehavior,
}

impl DigitalTrigger {
    pub fn new(bit_states: [BitState; 9], behavior: DigitalTriggerBehavior) -> Self {
        Self {
            bit_states,
            behavior,
        }
    }

    pub fn start_capturing_when() -> BitTriggerBuilder {
        BitTriggerBuilder::new()
    }
}

impl TriggerConfig for DigitalTrigger {
    fn into_trigger_fields(self) -> StringifiedTriggerConfig {
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
        StringifiedTriggerConfig {
            trigger_fields: format!(
                "{}0x{:02x} 0x{:02x}",
                trigger_behavior_flag, active_bits, relevant_bits
            ),
        }
    }
}

#[derive(Debug)]
pub struct AnalogTriggerBuilder {
    volts: f64,
    behavior: AnalogTriggerBehavior,
}

impl AnalogTriggerBuilder {
    pub fn rising_edge(mut self) -> AnalogTriggerBuilder {
        self.behavior = AnalogTriggerBehavior::Rising;
        self
    }

    pub fn falling_edge(mut self) -> AnalogTriggerBuilder {
        self.behavior = AnalogTriggerBehavior::Falling;
        self
    }

    pub fn level(mut self) -> AnalogTriggerBuilder {
        self.behavior = AnalogTriggerBehavior::Level;
        self
    }

    /// Same as level, but will also trigger when the voltage did not match within 100ms.
    pub fn auto(mut self) -> AnalogTriggerBuilder {
        self.behavior = AnalogTriggerBehavior::Auto;
        self
    }

    pub fn into_trigger(self, flea_probe: &FleaProbe) -> Result<AnalogTrigger, CaptureConfigError> {
        let raw_level = (flea_probe.voltage_to_raw(self.volts) / 4.0 + 0.5) as i16;

        if !(-1023..=1023).contains(&raw_level) {
            return Err(CaptureConfigError::VoltageOutOfRange);
        }
        Ok(AnalogTrigger::new(raw_level, self.behavior))
    }
}

#[derive(Debug, Clone)]
pub struct AnalogTrigger {
    pub level: i16,
    pub behavior: AnalogTriggerBehavior,
}

impl AnalogTrigger {
    pub fn new(raw_value: i16, behavior: AnalogTriggerBehavior) -> Self {
        Self {
            level: raw_value,
            behavior,
        }
    }

    pub fn start_capturing_when(volts: f64) -> AnalogTriggerBuilder {
        AnalogTriggerBuilder {
            volts,
            behavior: AnalogTriggerBehavior::Auto,
        }
    }
}

impl TriggerConfig for AnalogTrigger {
    fn into_trigger_fields(self) -> StringifiedTriggerConfig {
        let trigger_behavior_flag = self.behavior.as_str();

        StringifiedTriggerConfig {
            trigger_fields: format!("{}{} 0", trigger_behavior_flag, self.level),
        }
    }
}

/// A unified trigger type that can represent both analog and digital triggers.
/// This allows treating all triggers uniformly in the API.
#[derive(Debug, Clone)]
pub enum Trigger {
    Analog(AnalogTrigger),
    Digital(DigitalTrigger),
}

impl From<AnalogTrigger> for Trigger {
    fn from(trigger: AnalogTrigger) -> Self {
        Self::Analog(trigger)
    }
}

impl From<DigitalTrigger> for Trigger {
    fn from(trigger: DigitalTrigger) -> Self {
        Self::Digital(trigger)
    }
}
