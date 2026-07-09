use std::fmt::{Display, Formatter};
use std::str::FromStr;
use serde::{Deserialize, Serialize};
use crate::emulator::RomWritePolicy;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum WritePolicySpec {
    Ignore,
    Error,
}

impl WritePolicySpec {

    pub fn to_rom_write_policy(&self) -> RomWritePolicy {
        match self {
            WritePolicySpec::Ignore => RomWritePolicy::Ignore,
            WritePolicySpec::Error => RomWritePolicy::Error,
        }
    }

}

impl Display for WritePolicySpec {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            WritePolicySpec::Ignore => write!(f, "ignore"),
            WritePolicySpec::Error => write!(f, "error"),
        }
    }
}

impl From<WritePolicySpec> for String {
    fn from(v: WritePolicySpec) -> Self { v.to_string() }
}

impl TryFrom<String> for WritePolicySpec {
    type Error = String;

    fn try_from(s: String) -> Result<Self, <WritePolicySpec as TryFrom<String>>::Error> { s.parse() }
}

impl FromStr for WritePolicySpec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower_s = s.to_ascii_lowercase();
        let ls = lower_s.as_str();
        match ls {
            "ignore" => Ok(WritePolicySpec::Ignore),
            "error" => Ok(WritePolicySpec::Error),
            _ => Err(format!("Invalid write policy '{s}'; try 'ignore' or 'error'")),
        }
    }

}