use crate::{FleaProbe, flea_scope::CaptureConfigError};

pub struct StringifiedTriggerConfig {
    trigger_fields: String,
}

impl StringifiedTriggerConfig {
    pub fn into_string(self) -> String {
        self.trigger_fields
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitState {
    High,
    Low,
    DontCare,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DigitalTriggerBehavior {
    Auto,
    While,
    Start,
    Stop,
}

impl DigitalTriggerBehavior {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "~",
            Self::While => "",
            Self::Start => "+",
            Self::Stop => "-",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalogTriggerBehavior {
    Auto,
    Level,
    Rising,
    Falling,
}

impl AnalogTriggerBehavior {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "~",
            Self::Level => "",
            Self::Rising => "+",
            Self::Falling => "-",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Bit index {bit} out of range, must be between 0 and 8")]
pub struct BitIndexOutOfRange {
    pub bit: usize,
}

#[derive(Debug)]
#[must_use]
pub struct BitTriggerBuilder {
    bit_states: [BitState; 9],
}

impl BitTriggerBuilder {
    pub fn new() -> Self {
        Self {
            bit_states: [BitState::DontCare; 9],
        }
    }

    pub fn set_bit(mut self, bit: usize, state: BitState) -> Result<Self, BitIndexOutOfRange> {
        let slot = self
            .bit_states
            .get_mut(bit)
            .ok_or(BitIndexOutOfRange { bit })?;
        *slot = state;
        Ok(self)
    }

    pub fn bit0(mut self, state: BitState) -> Self {
        self.bit_states[0] = state;
        self
    }

    pub fn bit1(mut self, state: BitState) -> Self {
        self.bit_states[1] = state;
        self
    }

    pub fn bit2(mut self, state: BitState) -> Self {
        self.bit_states[2] = state;
        self
    }

    pub fn bit3(mut self, state: BitState) -> Self {
        self.bit_states[3] = state;
        self
    }

    pub fn bit4(mut self, state: BitState) -> Self {
        self.bit_states[4] = state;
        self
    }

    pub fn bit5(mut self, state: BitState) -> Self {
        self.bit_states[5] = state;
        self
    }

    pub fn bit6(mut self, state: BitState) -> Self {
        self.bit_states[6] = state;
        self
    }

    pub fn bit7(mut self, state: BitState) -> Self {
        self.bit_states[7] = state;
        self
    }

    pub fn bit8(mut self, state: BitState) -> Self {
        self.bit_states[8] = state;
        self
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

    /// Same as `is_matching`, but will also trigger when the bits did not match within 100ms.
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

    pub fn into_trigger_fields(self) -> StringifiedTriggerConfig {
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
                "{trigger_behavior_flag}0x{active_bits:02x} 0x{relevant_bits:02x}"
            ),
        }
    }
}

#[derive(Debug, Clone)]
#[must_use]
pub struct AnalogTriggerBuilder {
    pub volts: f64,
    pub behavior: AnalogTriggerBehavior,
}

impl AnalogTriggerBuilder {
    pub fn rising_edge(mut self) -> Self {
        self.behavior = AnalogTriggerBehavior::Rising;
        self
    }

    pub fn falling_edge(mut self) -> Self {
        self.behavior = AnalogTriggerBehavior::Falling;
        self
    }

    pub fn level(mut self) -> Self {
        self.behavior = AnalogTriggerBehavior::Level;
        self
    }

    /// Same as level, but will also trigger when the voltage did not match within 100ms.
    pub fn auto(mut self) -> Self {
        self.behavior = AnalogTriggerBehavior::Auto;
        self
    }

    pub fn into_trigger(self, flea_probe: &FleaProbe) -> Result<AnalogTrigger, CaptureConfigError> {
        let raw_level = (flea_probe.voltage_to_raw(self.volts)? / 4.0 + 0.5) as i16;

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

    pub fn into_trigger_fields(self) -> StringifiedTriggerConfig {
        let trigger_behavior_flag = self.behavior.as_str();

        StringifiedTriggerConfig {
            trigger_fields: format!("{}{} 0", trigger_behavior_flag, self.level),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_bit_accepts_last_bit_and_rejects_out_of_range() {
        let trigger = BitTriggerBuilder::new()
            .set_bit(8, BitState::High)
            .unwrap()
            .is_matching();
        assert_eq!(trigger.bit_states[8], BitState::High);

        let err = BitTriggerBuilder::new()
            .set_bit(9, BitState::High)
            .unwrap_err();
        assert_eq!(err, BitIndexOutOfRange { bit: 9 });
    }
}
