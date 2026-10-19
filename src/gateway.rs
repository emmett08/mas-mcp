//! Stable application facade composing publication policy with durable execution.
use crate::{
    catalog::Catalog,
    config::Config,
    digest,
    execution::Execution,
    host::{file_digest, path, read_bounded, Host},
    identifier,
    journal::Journal,
    Result, MAX_RESULT,
};
use serde_json::{json, Value};
use std::sync::Arc;

pub struct Gateway {
    catalog: Catalog,
    execution: Arc<Execution>,
    allow_payloads: bool,
}

impl Gateway {
    pub async fn open(config: Config) -> Result<Arc<Self>> {
        config.validate()?;
        let bytes = read_bounded(&config.bundle, 4 * MAX_RESULT)?;
        let bundle: Value = serde_json::from_slice(&bytes).map_err(|_| "invalid bundle JSON")?;
        let seconds = bundle["topology"]["wall_seconds"]
            .as_u64()
            .ok_or("bundle wall_seconds missing")?;
        if !(1..=3600).contains(&seconds) {
            return Err("bundle deadline outside 1..3600 seconds".into());
        }
        let host = Host {
            executable: config.host.clone(),
            bundle: config.bundle.clone(),
            state: config.state.join("runs"),
            host_digest: file_digest(&config.host)?,
            bundle_digest: digest(&bytes),
            seconds,
        };
        let described = host
            .invoke(&["describe", "--bundle", path(&config.bundle)?], None, 15)
            .await?;
        let catalog = Catalog::new(&config, &bundle, &described)?;
        let server_digest = file_digest(&std::env::current_exe().map_err(|e| e.to_string())?)?;
        let binding = json!({"config":config,"host":host.host_digest,"bundle":host.bundle_digest,"server":server_digest}).to_string();
        let journal = Journal::open(&config.state, &binding, config.max_active, config.max_tasks)?;
        Ok(Arc::new(Self {
            catalog,
            execution: Execution::new(host, journal),
            allow_payloads: config.allow_payloads,
        }))
    }

    pub fn describe(&self) -> Value {
        self.catalog.describe()
    }

    pub fn submit(
        self: &Arc<Self>,
        id: &str,
        operation: &str,
        input: &str,
        descriptor: &str,
    ) -> Result<Value> {
        if !identifier(id) {
            return Err("invalid request identity".into());
        }
        let invocation = self.catalog.prepare(operation, input, descriptor)?;
        self.execution.submit(id, operation, invocation)
    }

    pub fn status(&self, id: &str) -> Result<Value> {
        self.execution.status(id)
    }
    pub async fn cancel(&self, id: &str) -> Result<Value> {
        self.execution.cancel(id).await
    }

    pub async fn observe(&self, id: &str, payloads: bool) -> Result<Value> {
        self.execution.status(id)?;
        if payloads && !self.allow_payloads {
            return Err("payload disclosure is not permitted".into());
        }
        let observed = self.execution.host.observe(id, payloads).await?;
        Ok(json!({"task_id":id,"observation_json":observed.to_string()}))
    }

    pub async fn drain(&self) {
        self.execution.drain().await;
    }
}
