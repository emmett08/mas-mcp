//! Bounded CLI invocations of the trusted, immutable MAS host executable.
use crate::{digest, Result};
use serde_json::Value;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

pub(crate) fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("file exceeds size limit".into());
    }
    Ok(bytes)
}

pub(crate) fn file_digest(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let size = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[derive(Clone)]
pub(crate) struct Host {
    pub executable: PathBuf,
    pub bundle: PathBuf,
    pub state: PathBuf,
    pub host_digest: String,
    pub bundle_digest: String,
    pub seconds: u64,
}

impl Host {
    pub fn check_identity(&self) -> Result<()> {
        if file_digest(&self.executable)? != self.host_digest
            || digest(&read_bounded(&self.bundle, 4 * crate::MAX_RESULT)?) != self.bundle_digest
        {
            return Err("host or bundle changed; restore the retained deployment".into());
        }
        Ok(())
    }

    pub async fn invoke(
        &self,
        arguments: &[&str],
        input: Option<&str>,
        seconds: u64,
    ) -> Result<Value> {
        self.check_identity()?;
        let mut child = Command::new(&self.executable)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| "host could not start")?;
        let mut stdin = child.stdin.take().ok_or("host stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("host stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("host stderr unavailable")?;
        let operation = async {
            let write = async {
                if let Some(input) = input {
                    stdin
                        .write_all(input.as_bytes())
                        .await
                        .map_err(|_| "host input failed")?;
                }
                drop(stdin);
                Ok::<_, String>(())
            };
            let (_, output, _diagnostic, status) = tokio::try_join!(
                write,
                bounded_output(stdout),
                bounded_output(stderr),
                async {
                    child
                        .wait()
                        .await
                        .map_err(|_| "host wait failed".to_owned())
                }
            )?;
            if !status.success() {
                return Err("host returned an unsuccessful or uncertain outcome".into());
            }
            if arguments.first() == Some(&"run") {
                let text =
                    std::str::from_utf8(&output).map_err(|_| "host returned invalid UTF-8")?;
                mas_protocol_codec::parse_json(text)
                    .map_err(|_| "host returned invalid exact JSON".into())
            } else {
                // Observation metadata includes fractional Unix timestamps.
                serde_json::from_slice(&output).map_err(|_| "host returned invalid JSON".into())
            }
        };
        tokio::time::timeout(Duration::from_secs(seconds), operation)
            .await
            .map_err(|_| "host response deadline exceeded".to_owned())?
    }

    pub async fn run(&self, id: &str, agent: &str, input: &str) -> Result<Value> {
        self.invoke(
            &[
                "run",
                "--bundle",
                path(&self.bundle)?,
                "--agent",
                agent,
                "--input",
                "-",
                "--state",
                path(&self.state)?,
                "--run",
                id,
            ],
            Some(input),
            self.seconds + 10,
        )
        .await
    }

    pub async fn observe(&self, id: &str, payloads: bool) -> Result<Value> {
        self.invoke(
            &[
                "observe",
                "--state",
                path(&self.state)?,
                "--run",
                id,
                "--include-payloads",
                if payloads { "true" } else { "false" },
            ],
            None,
            10,
        )
        .await
    }

    pub async fn cancel(&self, id: &str) -> Result<Value> {
        self.invoke(
            &["cancel", "--state", path(&self.state)?, "--run", id],
            None,
            10,
        )
        .await
    }
}

pub(crate) fn path(value: &Path) -> Result<&str> {
    value.to_str().ok_or_else(|| "path is not UTF-8".into())
}

async fn bounded_output(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(crate::MAX_RESULT as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| "host output failed")?;
    if bytes.len() > crate::MAX_RESULT {
        return Err("host output exceeds limit".into());
    }
    Ok(bytes)
}
