//! Publication policy and exact application contracts, independent of MCP transport.
use crate::{config::Config, digest, Result, MAX_INPUT};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(crate) struct Catalog {
    descriptor: Value,
    contracts: BTreeMap<String, Value>,
}

pub(crate) struct Invocation {
    pub agent: String,
    pub input: String,
    output_schema: Value,
}

impl Catalog {
    pub fn new(config: &Config, bundle: &Value, described: &Value) -> Result<Self> {
        if described["version"] != "mas-host-descriptor/1"
            || described["numeric_profile"] != "exact-integer-json/1"
        {
            return Err("unsupported host descriptor".into());
        }
        let agents = described["agents"]
            .as_array()
            .ok_or("host contracts missing")?;
        let edges = bundle["topology"]["edges"]
            .as_array()
            .ok_or("bundle routes missing")?;
        let mut contracts = BTreeMap::new();
        let mut operations = Vec::new();
        for export in &config.exports {
            let contract = agents
                .iter()
                .find(|v| v["name"] == export.agent)
                .ok_or("export actor missing")?;
            for schema in ["input_schema", "output_schema"] {
                if contract[schema]["type"] != "object" {
                    return Err("exports require explicit object schemas".into());
                }
                mas_host_schema::check(&contract[schema])?;
            }
            if !edges.contains(&json!(["client", export.agent]))
                || !edges.contains(&json!([export.agent, "client"]))
            {
                return Err("export requires both client request and response routes".into());
            }
            operations.push(json!({"name":export.name,"description":export.description,"operation":contract["operation"],
                "input_schema":contract["input_schema"],"output_schema":contract["output_schema"]}));
            contracts.insert(export.name.clone(), contract.clone());
        }
        let mut descriptor = json!({"version":"mas-mcp-descriptor/1", "bundle_digest":described["bundle_digest"],
            "numeric_profile":"exact-integer-json/1", "max_input_bytes":MAX_INPUT, "operations":operations,
            "payload_observation_permitted":config.allow_payloads});
        descriptor["descriptor_digest"] = json!(digest(descriptor.to_string().as_bytes()));
        if descriptor.to_string().len() > crate::MAX_FRAME {
            return Err("published descriptor too large".into());
        }
        Ok(Self {
            descriptor,
            contracts,
        })
    }

    pub fn describe(&self) -> Value {
        self.descriptor.clone()
    }

    pub fn prepare(&self, operation: &str, input: &str, descriptor: &str) -> Result<Invocation> {
        if self.descriptor["descriptor_digest"] != descriptor {
            return Err("descriptor changed; inspect mas_describe".into());
        }
        let contract = self
            .contracts
            .get(operation)
            .ok_or("operation is not published")?;
        if input.len() > MAX_INPUT {
            return Err("application input exceeds limit".into());
        }
        let parsed = mas_protocol_codec::parse_json(input)
            .map_err(|_| "input_json must contain exact-integer JSON without duplicate keys")?;
        mas_host_schema::validate(&contract["input_schema"], &parsed)
            .map_err(|_| "input does not satisfy the published schema")?;
        Ok(Invocation {
            agent: contract["name"]
                .as_str()
                .ok_or("actor name missing")?
                .to_owned(),
            input: parsed.to_string(),
            output_schema: contract["output_schema"].clone(),
        })
    }
}

impl Invocation {
    pub fn completed_output(&self, id: &str, result: Value) -> Option<String> {
        if result["status"] != "completed" || result["run_id"] != id {
            return None;
        }
        let text = result.get("output")?.to_string();
        let value = mas_protocol_codec::parse_json(&text).ok()?;
        mas_host_schema::validate(&self.output_schema, &value).ok()?;
        if text.len() > MAX_INPUT {
            return None;
        }
        Some(text)
    }
}
