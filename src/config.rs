//! Operator-owned paths, export grants and resource limits.
use crate::{identifier, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub name: String,
    pub agent: String,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: String,
    pub host: PathBuf,
    pub bundle: PathBuf,
    pub state: PathBuf,
    pub exports: Vec<Export>,
    #[serde(default = "active_default")]
    pub max_active: usize,
    #[serde(default = "tasks_default")]
    pub max_tasks: usize,
    #[serde(default)]
    pub allow_payloads: bool,
}
fn active_default() -> usize {
    4
}
fn tasks_default() -> usize {
    1000
}

impl Config {
    pub fn read(path: &Path) -> Result<Self> {
        let path = path.canonicalize().map_err(|e| e.to_string())?;
        let bytes = crate::host::read_bounded(&path, crate::MAX_FRAME)?;
        // Use the protocol parser to reject duplicate configuration keys too.
        let value =
            mas_protocol_codec::parse_json(std::str::from_utf8(&bytes).map_err(|e| e.to_string())?)
                .map_err(|e| format!("invalid configuration: {e:?}"))?;
        let mut config: Self = serde_json::from_value(value).map_err(|e| e.to_string())?;
        let base = path.parent().ok_or("configuration has no parent")?;
        for location in [&mut config.host, &mut config.bundle, &mut config.state] {
            if location.is_relative() {
                *location = base.join(&*location);
            }
        }
        config.host = config.host.canonicalize().map_err(|e| e.to_string())?;
        config.bundle = config.bundle.canonicalize().map_err(|e| e.to_string())?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != "mas-mcp/1" {
            return Err("unsupported configuration version".into());
        }
        if !(1..=16).contains(&self.max_active) || !(1..=10000).contains(&self.max_tasks) {
            return Err("max_active must be 1..16 and max_tasks 1..10000".into());
        }
        if self.exports.is_empty() || self.exports.len() > 64 {
            return Err("publish 1..64 operations".into());
        }
        let mut names = BTreeSet::new();
        for export in &self.exports {
            if !identifier(&export.name)
                || !identifier(&export.agent)
                || !names.insert(&export.name)
                || export.description.is_empty()
                || export.description.len() > 2048
            {
                return Err("invalid or duplicate export".into());
            }
        }
        Ok(())
    }
}
