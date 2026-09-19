use std::collections::HashMap;
use std::fmt;

use jsonschema::{Draft, Validator};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{AgentError, AgentResult};
use crate::session::Secret;

const EMBEDDED_MANIFEST: &str = include_str!("../contract/agent-contract.json");
const MAX_ISSUES: usize = 8;
const MANIFEST_FORMAT: u32 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProtocolVersion {
    pub major: u32,
    pub minor: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Compatibility {
    pub protocol: ProtocolVersion,
    pub contract_digest: String,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingEnvelope {
    pub version: u8,
    pub port: u16,
    pub pairing_token: Secret,
    pub expected_origin: String,
    pub expires_at: u64,
    pub compatibility: Compatibility,
}

impl fmt::Debug for PairingEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingEnvelope")
            .field("version", &self.version)
            .field("port", &self.port)
            .field("pairing_token", &"[REDACTED]")
            .field("expected_origin", &self.expected_origin)
            .field("expires_at", &self.expires_at)
            .field("compatibility", &self.compatibility)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    format_version: u32,
    protocol: ProtocolVersion,
    contract_digest: String,
    transport: TransportSchemas,
    capabilities: Vec<CapabilitySchema>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransportSchemas {
    pairing_envelope: Value,
    command_envelope: Value,
    command_result_envelope: Value,
}

#[derive(Debug, Deserialize)]
struct CapabilitySchema {
    id: String,
    input: Value,
}

pub struct NativeContract {
    protocol: ProtocolVersion,
    contract_digest: String,
    pairing: Validator,
    command: Validator,
    result: Validator,
    capability_inputs: HashMap<String, Validator>,
}

impl fmt::Debug for NativeContract {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeContract")
            .field("protocol", &self.protocol)
            .field("contract_digest", &self.contract_digest)
            .field("capability_count", &self.capability_inputs.len())
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationIssue {
    pub path: String,
    pub message: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractValidationError {
    pub code: &'static str,
    pub message: &'static str,
    pub issues: Vec<ValidationIssue>,
}

impl fmt::Display for ContractValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ContractValidationError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatibilityError {
    kind: &'static str,
}

impl CompatibilityError {
    pub fn kind(&self) -> &'static str {
        self.kind
    }
}

impl fmt::Display for CompatibilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.kind {
            "protocol_mismatch" => "The local bridge uses a different GifGun protocol.",
            _ => "The local bridge was built from a different GifGun editor contract.",
        })
    }
}

impl std::error::Error for CompatibilityError {}

impl NativeContract {
    pub fn load_embedded() -> AgentResult<Self> {
        Self::from_json(EMBEDDED_MANIFEST)
    }

    pub fn from_json(source: &str) -> AgentResult<Self> {
        let raw: Value = serde_json::from_str(source)
            .map_err(|error| AgentError::Manifest(error.to_string()))?;
        reject_external_references(&raw, "contract")?;
        let manifest: Manifest =
            serde_json::from_value(raw).map_err(|error| AgentError::Manifest(error.to_string()))?;
        if manifest.format_version != MANIFEST_FORMAT {
            return Err(AgentError::Manifest(format!(
                "unsupported format version {}",
                manifest.format_version
            )));
        }
        if manifest.contract_digest.is_empty() {
            return Err(AgentError::Manifest(
                "the contract digest is empty".to_owned(),
            ));
        }

        let pairing = compile_schema(&manifest.transport.pairing_envelope, "pairing")?;
        let command = compile_schema(&manifest.transport.command_envelope, "command")?;
        let result = compile_schema(
            &manifest.transport.command_result_envelope,
            "command result",
        )?;
        let mut capability_inputs = HashMap::new();
        for capability in manifest.capabilities {
            let id = capability.id;
            if capability_inputs.contains_key(&id) {
                return Err(AgentError::Manifest(format!("duplicate capability {id}")));
            }
            capability_inputs.insert(
                id.clone(),
                compile_schema(&capability.input, &format!("{id} input"))?,
            );
        }

        Ok(Self {
            protocol: manifest.protocol,
            contract_digest: manifest.contract_digest,
            pairing,
            command,
            result,
            capability_inputs,
        })
    }

    pub fn digest(&self) -> &str {
        &self.contract_digest
    }

    pub fn protocol(&self) -> &ProtocolVersion {
        &self.protocol
    }

    pub fn compatibility(&self) -> Compatibility {
        Compatibility {
            protocol: self.protocol.clone(),
            contract_digest: self.contract_digest.clone(),
        }
    }

    pub fn check_compatibility(&self, received: &Compatibility) -> Result<(), CompatibilityError> {
        if received.protocol != self.protocol {
            return Err(CompatibilityError {
                kind: "protocol_mismatch",
            });
        }
        if received.contract_digest != self.contract_digest {
            return Err(CompatibilityError {
                kind: "contract_mismatch",
            });
        }
        Ok(())
    }

    pub fn validate_pairing(&self, value: &Value) -> Result<(), ContractValidationError> {
        validate(&self.pairing, value)
    }

    pub fn validate_command(&self, value: &Value) -> Result<(), ContractValidationError> {
        validate(&self.command, value)?;
        let capability_id = value
            .get("capabilityId")
            .and_then(Value::as_str)
            .ok_or_else(invalid_contract)?;
        let input = value.get("input").ok_or_else(invalid_contract)?;
        self.validate_capability_input(capability_id, input)
    }

    pub fn validate_result(&self, value: &Value) -> Result<(), ContractValidationError> {
        validate(&self.result, value)
    }

    pub fn validate_capability_input(
        &self,
        capability_id: &str,
        value: &Value,
    ) -> Result<(), ContractValidationError> {
        let validator =
            self.capability_inputs
                .get(capability_id)
                .ok_or_else(|| ContractValidationError {
                    code: "unknown_capability",
                    message: "The connected editor does not support this capability.",
                    issues: Vec::new(),
                })?;
        validate(validator, value)
    }
}

fn compile_schema(schema: &Value, name: &str) -> AgentResult<Validator> {
    jsonschema::options()
        .with_draft(Draft::Draft7)
        .build(schema)
        .map_err(|_| AgentError::Schema(format!("{name} schema is invalid")))
}

fn validate(validator: &Validator, value: &Value) -> Result<(), ContractValidationError> {
    if validator.is_valid(value) {
        return Ok(());
    }
    let issues = validator
        .iter_errors(value)
        .take(MAX_ISSUES)
        .map(|error| ValidationIssue {
            path: {
                let path = error.instance_path().to_string();
                if path.is_empty() {
                    "/".to_owned()
                } else {
                    path
                }
            },
            message: "does not match the editor runtime contract",
        })
        .collect();
    Err(ContractValidationError {
        code: "validation",
        message: "The request does not match the editor runtime contract.",
        issues,
    })
}

fn invalid_contract() -> ContractValidationError {
    ContractValidationError {
        code: "validation",
        message: "The request does not match the editor runtime contract.",
        issues: Vec::new(),
    }
}

fn reject_external_references(value: &Value, path: &str) -> AgentResult<()> {
    match value {
        Value::Object(record) => {
            if record.contains_key("$ref") {
                return Err(AgentError::Manifest(format!(
                    "external schema reference at {path}"
                )));
            }
            for (key, nested) in record {
                reject_external_references(nested, &format!("{path}/{key}"))?;
            }
        }
        Value::Array(items) => {
            for (index, nested) in items.iter().enumerate() {
                reject_external_references(nested, &format!("{path}/{index}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}
