//! Durable request identity, capacity reservations and conservative task transitions.
use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

pub(crate) struct Journal {
    connection: Connection,
    _owner: File,
    max_active: usize,
    max_tasks: usize,
}

impl Journal {
    pub fn open(state: &Path, binding: &str, max_active: usize, max_tasks: usize) -> Result<Self> {
        if !state.exists() {
            std::fs::create_dir_all(state).map_err(|e| e.to_string())?;
            std::fs::set_permissions(state, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        if std::fs::metadata(state)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err("state directory must be private (chmod 700)".into());
        }
        let owner = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(state.join("owner.lock"))
            .map_err(|e| e.to_string())?;
        owner
            .try_lock()
            .map_err(|_| "state directory already has a serving owner")?;
        let connection = Connection::open(state.join("gateway.sqlite3")).map_err(db_error)?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS binding (id INTEGER PRIMARY KEY CHECK(id=1), value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY, operation TEXT NOT NULL, input TEXT NOT NULL,
                state TEXT NOT NULL CHECK(state IN ('submitted','working','completed','unknown','cancelled')),
                cancel_requested INTEGER NOT NULL DEFAULT 0, output TEXT);").map_err(db_error)?;
        let prior: Option<String> = connection
            .query_row("SELECT value FROM binding WHERE id=1", [], |r| r.get(0))
            .optional()
            .map_err(db_error)?;
        match prior {
            Some(prior) if prior != binding => {
                return Err(
                    "state belongs to a different server, host, bundle or configuration".into(),
                )
            }
            None => {
                connection
                    .execute("INSERT INTO binding VALUES(1, ?1)", [binding])
                    .map_err(db_error)?;
            }
            _ => {}
        }
        connection
            .execute(
                "UPDATE tasks SET state='unknown' WHERE state IN ('submitted','working')",
                [],
            )
            .map_err(db_error)?;
        Ok(Self {
            connection,
            _owner: owner,
            max_active,
            max_tasks,
        })
    }

    pub fn submit(&mut self, id: &str, operation: &str, input: &str) -> Result<(Value, bool)> {
        let transaction = self.connection.transaction().map_err(db_error)?;
        let previous: Option<(String, String)> = transaction
            .query_row("SELECT operation,input FROM tasks WHERE id=?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(db_error)?;
        if let Some((old_operation, old_input)) = previous {
            if operation != old_operation || input != old_input {
                return Err("request identity already binds different content".into());
            }
            drop(transaction);
            return Ok((self.status(id)?, false));
        }
        let (total, active): (i64, i64) = transaction.query_row(
            "SELECT COUNT(*), COALESCE(SUM(state IN ('submitted','working','unknown')),0) FROM tasks", [],
            |r| Ok((r.get(0)?,r.get(1)?))).map_err(db_error)?;
        if total >= self.max_tasks as i64 {
            return Err("lifetime request capacity exhausted".into());
        }
        if active >= self.max_active as i64 {
            return Err("active or unresolved request capacity exhausted".into());
        }
        transaction
            .execute(
                "INSERT INTO tasks(id,operation,input,state) VALUES(?1,?2,?3,'submitted')",
                params![id, operation, input],
            )
            .map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        Ok((self.status(id)?, true))
    }

    /// The winner of this transaction may dispatch once. Cancellation may win first.
    pub fn begin(&mut self, id: &str) -> Result<bool> {
        self.connection.execute("UPDATE tasks SET state='working' WHERE id=?1 AND state='submitted' AND cancel_requested=0", [id])
            .map(|count| count == 1).map_err(db_error)
    }

    pub fn finish(&mut self, id: &str, output: Option<&str>) -> Result<()> {
        self.connection
            .execute(
                "UPDATE tasks SET state=?2,output=?3 WHERE id=?1 AND state='working'",
                params![
                    id,
                    if output.is_some() {
                        "completed"
                    } else {
                        "unknown"
                    },
                    output
                ],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn cancel(&mut self, id: &str) -> Result<Value> {
        // A submitted request can be confirmed cancelled because begin() can no longer win.
        let count = self
            .connection
            .execute(
                "UPDATE tasks SET cancel_requested=1,
            state=CASE WHEN state='submitted' THEN 'cancelled' ELSE state END WHERE id=?1",
                [id],
            )
            .map_err(db_error)?;
        if count == 0 {
            return Err("unknown task identity".into());
        }
        self.status(id)
    }

    pub fn status(&self, id: &str) -> Result<Value> {
        self.connection.query_row("SELECT operation,state,cancel_requested,output FROM tasks WHERE id=?1", [id], |r| {
            let output: Option<String> = r.get(3)?;
            let mut task = json!({"task_id":id, "operation":r.get::<_,String>(0)?, "state":r.get::<_,String>(1)?, "cancel_requested":r.get::<_,bool>(2)?});
            if let Some(output) = output { task["output_json"] = json!(output); }
            Ok(task)
        }).optional().map_err(db_error)?.ok_or_else(|| "unknown task identity".into())
    }
}

fn db_error(_: rusqlite::Error) -> String {
    "durable request journal unavailable".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn journal(dir: &Path) -> Journal {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        Journal::open(dir, "binding", 1, 3).unwrap()
    }
    #[test]
    fn restart_preserves_unknown_reservation_and_exact_identity() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = journal(dir.path());
        assert!(first.submit("a", "review", "{}").unwrap().1);
        assert!(first.begin("a").unwrap());
        assert!(!first.begin("a").unwrap());
        drop(first);
        let mut second = journal(dir.path());
        assert_eq!(second.status("a").unwrap()["state"], "unknown");
        assert!(!second.submit("a", "review", "{}").unwrap().1);
        assert!(second.submit("a", "review", "{\"changed\":1}").is_err());
        assert!(second.submit("b", "review", "{}").is_err());
        assert!(!second.begin("a").unwrap());
    }
    #[test]
    fn cancellation_before_dispatch_and_success_after_intent_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = journal(dir.path());
        journal.submit("a", "review", "{}").unwrap();
        assert_eq!(journal.cancel("a").unwrap()["state"], "cancelled");
        assert!(!journal.begin("a").unwrap());
        journal.submit("b", "review", "{}").unwrap();
        assert!(journal.begin("b").unwrap());
        assert_eq!(journal.cancel("b").unwrap()["state"], "working");
        journal.finish("b", Some("{\"approved\":false}")).unwrap();
        assert_eq!(journal.status("b").unwrap()["state"], "completed");
        assert_eq!(journal.status("b").unwrap()["cancel_requested"], true);
    }
    #[test]
    fn ownership_and_deployment_binding_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let first = journal(dir.path());
        assert!(Journal::open(dir.path(), "binding", 1, 3).is_err());
        drop(first);
        assert!(Journal::open(dir.path(), "changed", 1, 3).is_err());
    }
}
