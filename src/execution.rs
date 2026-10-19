//! Coordinate durable admission, one host dispatch and retained completion.
use crate::{catalog::Invocation, host::Host, journal::Journal, Result};
use serde_json::Value;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

pub(crate) struct Execution {
    pub host: Host,
    journal: Mutex<Journal>,
    running: AtomicUsize,
}

// Shutdown accounting must be released on normal return, cancellation and panic.
struct ActiveDispatch(Arc<Execution>);
impl Drop for ActiveDispatch {
    fn drop(&mut self) {
        self.0.running.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Execution {
    pub fn new(host: Host, journal: Journal) -> Arc<Self> {
        Arc::new(Self {
            host,
            journal: Mutex::new(journal),
            running: AtomicUsize::new(0),
        })
    }

    pub fn submit(
        self: &Arc<Self>,
        id: &str,
        operation: &str,
        invocation: Invocation,
    ) -> Result<Value> {
        let (task, fresh) = self
            .journal
            .lock()
            .map_err(|_| "journal lock failed")?
            .submit(id, operation, &invocation.input)?;
        if fresh {
            self.running.fetch_add(1, Ordering::SeqCst);
            let active = ActiveDispatch(self.clone());
            let runner = self.clone();
            let id = id.to_owned();
            tokio::spawn(async move {
                let _active = active;
                runner.execute(&id, &invocation).await;
            });
        }
        Ok(task)
    }

    async fn execute(&self, id: &str, invocation: &Invocation) {
        let admitted = self
            .journal
            .lock()
            .ok()
            .and_then(|mut j| j.begin(id).ok())
            .unwrap_or(false);
        if !admitted {
            return;
        }
        let execution = self.host.run(id, &invocation.agent, &invocation.input);
        tokio::pin!(execution);
        // Cancellation forwarding must never stop draining the running host's pipes.
        let cancellation = async {
            loop {
                if self.status(id).is_ok_and(|v| v["cancel_requested"] == true)
                    && self.host.cancel(id).await.is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        };
        tokio::pin!(cancellation);
        let result = tokio::select! {
            value = &mut execution => value,
            _ = &mut cancellation => execution.await,
        };
        let output = result
            .ok()
            .and_then(|result| invocation.completed_output(id, result));
        if let Ok(mut journal) = self.journal.lock() {
            if journal.finish(id, output.as_deref()).is_err() {
                eprintln!("could not retain task completion; restart will preserve uncertainty");
            }
        }
    }

    pub fn status(&self, id: &str) -> Result<Value> {
        self.journal
            .lock()
            .map_err(|_| "journal lock failed")?
            .status(id)
    }

    pub async fn cancel(&self, id: &str) -> Result<Value> {
        let task = self
            .journal
            .lock()
            .map_err(|_| "journal lock failed")?
            .cancel(id)?;
        if task["state"] == "unknown" {
            let _ = self.host.cancel(id).await;
        }
        // Intent is the promise, not external effect rollback or confirmed termination.
        self.status(id)
    }

    /// Accepted work keeps its original host deadline even after the MCP client disconnects.
    pub async fn drain(&self) {
        while self.running.load(Ordering::SeqCst) != 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}
