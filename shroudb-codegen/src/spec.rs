//! Protocol specification parser.
//!
//! Deserializes `protocol.toml` into typed Rust structs that generators consume.

use serde::Deserialize;
use std::collections::BTreeMap;

/// Root of the protocol specification.
#[derive(Debug, Deserialize)]
pub struct ProtocolSpec {
    pub protocol: ProtocolMeta,
    pub error_codes: BTreeMap<String, ErrorCodeDef>,
    pub types: BTreeMap<String, TypeDef>,
    pub commands: BTreeMap<String, CommandDef>,
}

#[derive(Debug, Deserialize)]
pub struct ProtocolMeta {
    pub name: String,
    pub version: String,
    pub description: String,
    pub default_port: u16,
    pub uri_schemes: Vec<String>,
    pub uri_format: String,
}

#[derive(Debug, Deserialize)]
pub struct ErrorCodeDef {
    pub description: String,
    pub http_equiv: u16,
}

#[derive(Debug, Deserialize)]
pub struct TypeDef {
    pub description: String,
    pub wire_type: String,
    pub rust_type: String,
    pub python_type: String,
    pub typescript_type: String,
}

#[derive(Debug, Deserialize)]
pub struct CommandDef {
    pub verb: String,
    #[serde(default)]
    pub subcommand: Option<String>,
    pub description: String,
    pub replica_behavior: String,
    #[serde(default)]
    pub variant: Option<String>,
    #[serde(default)]
    pub simple_response: bool,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub params: Vec<ParamDef>,
    #[serde(default)]
    pub response: Vec<ResponseFieldDef>,
    #[serde(default)]
    pub errors: Vec<ErrorRef>,
}

#[derive(Debug, Deserialize)]
pub struct ParamDef {
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub position: Option<u32>,
    #[serde(default)]
    pub key: Option<String>,
    pub description: String,
    #[serde(default)]
    pub variadic: bool,
}

#[derive(Debug, Deserialize)]
pub struct ResponseFieldDef {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    pub description: String,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Deserialize)]
pub struct ErrorRef {
    pub code: String,
    pub condition: String,
}

impl ProtocolSpec {
    /// Parse a protocol spec from TOML text.
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}

impl CommandDef {
    /// Positional params sorted by position.
    pub fn positional_params(&self) -> Vec<&ParamDef> {
        let mut positional: Vec<&ParamDef> = self
            .params
            .iter()
            .filter(|p| p.position.is_some())
            .collect();
        positional.sort_by_key(|p| p.position.unwrap());
        positional
    }

    /// Named (keyword) params — those with a `key` field.
    pub fn named_params(&self) -> Vec<&ParamDef> {
        self.params.iter().filter(|p| p.key.is_some()).collect()
    }

    /// Required response fields.
    pub fn required_response_fields(&self) -> Vec<&ResponseFieldDef> {
        self.response.iter().filter(|r| !r.optional).collect()
    }

    /// Optional response fields.
    pub fn optional_response_fields(&self) -> Vec<&ResponseFieldDef> {
        self.response.iter().filter(|r| r.optional).collect()
    }
}
