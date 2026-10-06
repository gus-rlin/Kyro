//! Versioned model envelopes. v1 is retained for durable jobs already admitted.
use kyro_domain::{Error, Result};
use serde_json::Value;

pub fn output_schema_v2() -> Value {
    serde_json::from_str(include_str!("contract-v2.schema.json")).expect("versioned native schema")
}

pub(crate) fn decode(version: &str, envelope: Value) -> Result<Value> {
    let mut fields = envelope
        .as_object()
        .filter(|fields| fields.len() == 1)
        .ok_or_else(|| Error::Invalid("invalid_agent_envelope".into()))?
        .clone();
    let contract = fields
        .remove("contract")
        .ok_or_else(|| Error::Invalid("invalid_agent_envelope".into()))?;
    let data = match version {
        "1" => {
            let text = contract
                .as_str()
                .ok_or_else(|| Error::Invalid("invalid_agent_envelope".into()))?;
            if text.len() > 32000 {
                return Err(Error::ResourceLimit);
            }
            serde_json::from_str(text)
                .map_err(|_| Error::Invalid("invalid_agent_contract".into()))?
        }
        "2" if contract.is_object() => {
            if serde_json::to_vec(&contract)
                .map_err(|_| Error::Internal)?
                .len()
                > 32000
            {
                return Err(Error::ResourceLimit);
            }
            contract
        }
        _ => return Err(Error::Invalid("invalid_agent_envelope".into())),
    };
    kyro_domain::model::reject_recognizable_secrets(&data)?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_contracts_preserve_json_and_legacy_jobs_without_version_coercion() {
        let contract =
            json!({"objective":r#"Quotes " and slash \"#,"tasks":[],"missing_capabilities":[]});
        assert_eq!(decode("2", json!({"contract":contract})).unwrap(), contract);
        assert_eq!(
            decode("1", json!({"contract":contract.to_string()})).unwrap(),
            contract
        );
        for (version, envelope) in [
            ("1", json!({"contract":contract})),
            ("2", json!({"contract":contract.to_string()})),
            ("2", json!({"contract":contract,"extra":true})),
            ("2", json!({"contract":null})),
            ("3", json!({"contract":contract})),
            ("1", json!({"contract":"?objective?:oops"})),
        ] {
            assert!(decode(version, envelope).is_err());
        }
        assert!(decode("2", json!({"contract":{"oversized":"a".repeat(32001)}})).is_err());
    }
}
