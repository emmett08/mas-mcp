//! Exercise the published binary through the same line-delimited stdio used by MCP clients.
#![cfg(target_os = "linux")]

use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt, process::Stdio, time::Duration};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    time::timeout,
};

const DEADLINE: Duration = Duration::from_secs(5);

struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr: ChildStderr,
    // Keep the executable, configuration and durable state alive until the process exits.
    _fixture: TempDir,
}

impl Client {
    async fn start() -> Self {
        let fixture = tempfile::tempdir().unwrap();
        let host = fixture.path().join("fake-host.py");
        fs::write(
            &host,
            r#"#!/usr/bin/env python3
import json, sys
schema = {"type": "object", "properties": {"number": {"type": "integer"}}, "required": ["number"], "additionalProperties": False}
print("host diagnostic must never appear on MCP stdout", file=sys.stderr)
if sys.argv[1] == "describe":
    print(json.dumps({"version": "mas-host-descriptor/1", "numeric_profile": "exact-integer-json/1", "bundle_digest": "fixture-bundle", "agents": [{"name": "echo", "operation": "echo", "input_schema": schema, "output_schema": schema}]}))
elif sys.argv[1] == "run":
    run_id = sys.argv[sys.argv.index("--run") + 1]
    print(json.dumps({"status": "completed", "run_id": run_id, "output": json.load(sys.stdin)}))
else:
    raise SystemExit("unexpected host command")
"#,
        )
        .unwrap();
        fs::set_permissions(&host, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(
            fixture.path().join("bundle.json"),
            json!({"topology":{"wall_seconds":1,"edges":[["client","echo"],["echo","client"]]}})
                .to_string(),
        )
        .unwrap();
        let config = fixture.path().join("config.json");
        fs::write(
            &config,
            json!({
                "version":"mas-mcp/1", "host":"fake-host.py", "bundle":"bundle.json", "state":"state",
                "exports":[{"name":"echo_number","agent":"echo","description":"Return the supplied exact integer."}]
            }).to_string(),
        ).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_mas-mcp"))
            .args(["--config", config.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let stderr = child.stderr.take().unwrap();
        Self {
            child,
            stdin,
            stdout,
            stderr,
            _fixture: fixture,
        }
    }

    async fn send(&mut self, message: Value) {
        let line = format!("{message}\n");
        timeout(
            DEADLINE,
            self.stdin.as_mut().unwrap().write_all(line.as_bytes()),
        )
        .await
        .expect("MCP write timed out")
        .expect("MCP write failed");
    }

    async fn response(&mut self, id: u64) -> Value {
        let mut line = String::new();
        let length = timeout(DEADLINE, self.stdout.read_line(&mut line))
            .await
            .expect("MCP response timed out")
            .expect("MCP response read failed");
        assert_ne!(length, 0, "MCP process closed stdout before responding");
        let message: Value = serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("non-JSON content on MCP stdout: {line:?}: {error}"));
        assert_eq!(message["jsonrpc"], "2.0");
        assert_eq!(message["id"], id, "unexpected MCP response: {message}");
        assert!(
            message.get("error").is_none(),
            "unexpected RPC error: {message}"
        );
        message["result"].clone()
    }

    async fn initialise(&mut self) {
        // 2025-11-25 is rmcp 3.5.1's latest revision with the initialise handshake.
        self.send(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-11-25","capabilities":{},
                "clientInfo":{"name":"mas-mcp-stdio-test","version":"1"}
            }}),
        )
        .await;
        let result = self.response(1).await;
        assert_eq!(result["protocolVersion"], "2025-11-25");
        assert_eq!(result["serverInfo"]["name"], "mas-mcp");
        assert!(result["capabilities"]["tools"].is_object());
        self.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
    }

    async fn call(&mut self, id: u64, name: &str, arguments: Value) -> Value {
        self.send(
            json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
                "name":name,"arguments":arguments
            }}),
        )
        .await;
        self.response(id).await
    }

    async fn finish(mut self) {
        drop(self.stdin.take());
        let status = timeout(DEADLINE, self.child.wait())
            .await
            .expect("MCP process did not exit after EOF")
            .unwrap();
        let mut remaining = String::new();
        self.stdout.read_to_string(&mut remaining).await.unwrap();
        let mut diagnostic = String::new();
        self.stderr.read_to_string(&mut diagnostic).await.unwrap();
        assert!(status.success(), "MCP process failed: {diagnostic}");
        assert!(
            remaining.is_empty(),
            "unsolicited stdout after final response: {remaining:?}"
        );
        assert!(
            !diagnostic.contains("host diagnostic"),
            "trusted host diagnostics leaked from the gateway"
        );
    }
}

#[tokio::test]
async fn stdio_negotiates_lists_tools_and_preserves_exact_output() {
    let mut client = Client::start().await;
    client.initialise().await;
    client
        .send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}))
        .await;
    let listed = client.response(2).await;
    let tools = listed["tools"].as_array().unwrap();
    let names: std::collections::BTreeSet<_> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "mas_cancel",
            "mas_describe",
            "mas_observe",
            "mas_status",
            "mas_submit"
        ]
        .into_iter()
        .collect()
    );
    for tool in tools {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    }
    let described = client.call(3, "mas_describe", json!({})).await;
    assert_ne!(described["isError"], true);
    let descriptor = &described["structuredContent"];
    assert_eq!(descriptor["operations"][0]["name"], "echo_number");
    assert_eq!(descriptor["operations"].as_array().unwrap().len(), 1);
    assert_eq!(descriptor["payload_observation_permitted"], false);
    let digest = descriptor["descriptor_digest"].as_str().unwrap();
    assert_eq!(digest.len(), 64);
    let input = r#"{"number":1234567890123456789012345678901234567890}"#;
    let submitted = client.call(4, "mas_submit", json!({
        "request_id":"exact-number", "operation":"echo_number", "input_json":input, "descriptor_digest":digest
    })).await;
    assert_ne!(submitted["isError"], true, "{submitted}");
    let task = submitted["structuredContent"]["task_id"]
        .as_str()
        .unwrap()
        .to_owned();
    timeout(DEADLINE, async {
        for id in 5.. {
            let status = client.call(id, "mas_status", json!({"task_id":task})).await;
            assert_ne!(status["isError"], true, "{status}");
            match status["structuredContent"]["state"].as_str().unwrap() {
                "completed" => {
                    assert_eq!(status["structuredContent"]["output_json"], input);
                    break;
                }
                "queued" | "working" => tokio::time::sleep(Duration::from_millis(10)).await,
                state => panic!("unexpected terminal state: {state}: {status}"),
            }
        }
    })
    .await
    .expect("submitted work did not complete");
    client.finish().await;
}

#[tokio::test]
async fn stdio_rejects_invalid_tool_arguments_and_remains_usable() {
    let mut client = Client::start().await;
    client.initialise().await;
    let invalid = [
        ("mas_describe", json!({"extra":true})),
        ("mas_status", json!({})),
        ("mas_status", json!({"task_id":42})),
        ("mas_cancel", json!({"task_id":"unknown","command":"sh"})),
        (
            "mas_observe",
            json!({"task_id":"unknown","include_payloads":"true"}),
        ),
        (
            "mas_submit",
            json!({"request_id":"x","operation":"echo_number","input_json":{},"descriptor_digest":"x"}),
        ),
        ("run_shell", json!({"command":"echo unexpected"})),
    ];
    for (offset, (tool, arguments)) in invalid.into_iter().enumerate() {
        let result = client.call(offset as u64 + 2, tool, arguments).await;
        assert_eq!(
            result["isError"], true,
            "invalid {tool} arguments accepted: {result}"
        );
        assert!(result["content"][0]["text"]
            .as_str()
            .is_some_and(|s| !s.is_empty()));
    }
    let valid = client.call(20, "mas_describe", json!({})).await;
    assert_ne!(valid["isError"], true);
    assert_eq!(
        valid["structuredContent"]["version"],
        "mas-mcp-descriptor/1"
    );
    client.finish().await;
}

#[tokio::test]
async fn oversized_unterminated_frame_is_bounded_without_waiting_for_eof() {
    let mut client = Client::start().await;
    client.initialise().await;
    // Deliberately leave stdin open and omit the delimiter. Rejecting only after
    // a newline would leave the server vulnerable to an indefinitely growing frame.
    let mut oversized = br#"{"jsonrpc":"2.0","id":99,"method":"tools/call","params":{"name":"mas_describe","arguments":{"padding":""#.to_vec();
    oversized.resize(mas_mcp::MAX_FRAME + 1, b'x');
    let write = timeout(
        DEADLINE,
        client.stdin.as_mut().unwrap().write_all(&oversized),
    )
    .await
    .expect("server blocked while receiving an oversized frame");
    // A bounded transport may close its pipe before all bytes reach it.
    if let Err(error) = write {
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    }
    timeout(DEADLINE, client.child.wait())
        .await
        .expect("oversized frame did not terminate the session while stdin remained open")
        .unwrap();
    drop(client.stdin.take());
    let mut remaining = String::new();
    client.stdout.read_to_string(&mut remaining).await.unwrap();
    for line in remaining.lines() {
        let response: Value =
            serde_json::from_str(line).expect("non-JSON diagnostics on MCP stdout");
        assert!(
            response.get("result").is_none(),
            "oversized request was executed: {response}"
        );
    }
}
