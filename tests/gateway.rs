//! Boundary tests use a local stand-in for the approved CLI, never an external provider.
use mas_mcp::{
    config::{Config, Export},
    gateway::Gateway,
    MAX_INPUT,
};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const FAKE_HOST: &str = r#"#!/usr/bin/env python3
import json, pathlib, sys, time
root = pathlib.Path(__file__).parent
cmd = sys.argv[1]
args = dict(zip(sys.argv[2::2], sys.argv[3::2]))
def log(event):
    with (root / 'calls').open('a') as out:
        out.write(json.dumps(event) + '\n')
if cmd == 'describe':
    print((root / 'descriptor.json').read_text())
elif cmd == 'run':
    run = args['--run']
    text = sys.stdin.read()
    request = json.loads(text)
    log({'command': cmd, 'run': run, 'agent': args['--agent'], 'input': text})
    mode = (root / 'mode').read_text()
    if mode == 'delayed-start':
        time.sleep(0.6)
    (root / ('started-' + run)).write_text('ready')
    if mode == 'slow-cancel':
        time.sleep(0.4)
    if mode in ('wait', 'delayed-start'):
        until = time.monotonic() + 5
        while time.monotonic() < until and not (root / ('cancel-' + run)).exists():
            time.sleep(0.01)
        if (root / ('cancel-' + run)).exists():
            print(json.dumps({'status': 'cancelled', 'run_id': run}))
            sys.exit(0)
    if mode == 'nonzero':
        sys.exit(7)
    if mode == 'oversized':
        print('x' * 1048577)
        sys.exit(0)
    if mode == 'duplicate-output':
        print('{"status":"completed","run_id":' + json.dumps(run) + ',"output":{"n":1,"n":2,"approved":false}}')
        sys.exit(0)
    output = {'n': request['n'], 'approved': False}
    if mode == 'invalid-output':
        output['n'] = 'not an integer'
    if mode == 'fractional-output':
        output['n'] = 0.5
    print(json.dumps({'status': 'completed', 'run_id': ('other' if mode == 'wrong-run' else run), 'output': output}))
elif cmd == 'cancel':
    run = args['--run']
    log({'command': cmd, 'run': run})
    if not (root / ('started-' + run)).exists():
        sys.exit(3)
    if (root / 'mode').read_text() == 'slow-cancel':
        time.sleep(2)
    (root / ('cancel-' + run)).write_text('requested')
    print(json.dumps({'cancel_requested': True}))
elif cmd == 'observe':
    log({'command': cmd, 'run': args['--run']})
    print(json.dumps({'run_id': args['--run'], 'timestamp': 0.25, 'include_payloads': args['--include-payloads'] == 'true'}))
else:
    sys.exit(2)
"#;

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    config: Config,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        let host = root.join("host.py");
        fs::write(&host, FAKE_HOST).unwrap();
        fs::set_permissions(&host, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.join("mode"), "success").unwrap();
        let bundle = root.join("bundle.json");
        fs::write(&bundle, json!({"topology":{"wall_seconds":2,"edges":[["client","checker"],["checker","client"]]}}).to_string()).unwrap();
        let input = json!({"type":"object","properties":{"n":{"type":"integer"}},"required":["n"],"additionalProperties":false});
        let output = json!({"type":"object","properties":{"n":{"type":"integer"},"approved":{"type":"boolean"}},"required":["n","approved"],"additionalProperties":false});
        fs::write(root.join("descriptor.json"), json!({"version":"mas-host-descriptor/1","numeric_profile":"exact-integer-json/1","bundle_digest":"fixture","agents":[{"name":"checker","operation":"check/1","input_schema":input,"output_schema":output}]}).to_string()).unwrap();
        let config = Config {
            version: "mas-mcp/1".into(),
            host,
            bundle,
            state: root.join("state"),
            exports: vec![Export {
                name: "review".into(),
                agent: "checker".into(),
                description: "Review the supplied integer".into(),
            }],
            max_active: 4,
            max_tasks: 1000,
            allow_payloads: false,
        };
        Self {
            _directory: directory,
            root,
            config,
        }
    }

    fn mode(&self, mode: &str) {
        fs::write(self.root.join("mode"), mode).unwrap();
    }
    fn calls(&self, command: &str) -> Vec<Value> {
        fs::read_to_string(self.root.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|value| value["command"] == command)
            .collect()
    }
    async fn open(&self) -> Arc<Gateway> {
        Gateway::open(self.config.clone()).await.unwrap()
    }
}

fn submit(gateway: &Arc<Gateway>, id: &str, input: &str) -> mas_mcp::Result<Value> {
    let descriptor = gateway.describe();
    gateway.submit(
        id,
        "review",
        input,
        descriptor["descriptor_digest"].as_str().unwrap(),
    )
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("condition was not reached within five seconds");
}

async fn drain(gateway: &Gateway) {
    tokio::time::timeout(Duration::from_secs(5), gateway.drain())
        .await
        .expect("task did not finish");
}

#[tokio::test]
async fn preserves_integer_digits_and_application_rejection_is_completed_execution() {
    let fixture = Fixture::new();
    let gateway = fixture.open().await;
    let number = "12345678901234567890123456789012345678901234567890";
    submit(&gateway, "exact", &format!("{{\"n\":{number}}}")).unwrap();
    drain(&gateway).await;
    let task = gateway.status("exact").unwrap();
    assert_eq!(task["state"], "completed");
    assert_eq!(
        task["output_json"],
        format!("{{\"approved\":false,\"n\":{number}}}")
    );
    let calls = fixture.calls("run");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["input"], format!("{{\"n\":{number}}}"));
    assert_eq!(calls[0]["agent"], "checker");
}

#[tokio::test]
async fn rejects_nonexact_or_schema_invalid_inputs_before_dispatch_or_reservation() {
    let fixture = Fixture::new();
    let gateway = fixture.open().await;
    for input in [
        "{\"n\":1,\"n\":2}",
        "{\"n\":0.1}",
        "{\"n\":1e2}",
        "{\"n\":true}",
        "{}",
        "{\"n\":1,\"extra\":2}",
        "[]",
    ] {
        assert!(
            submit(&gateway, "reusable", input).is_err(),
            "accepted {input}"
        );
        assert!(gateway.status("reusable").is_err());
    }
    let oversized = format!("{{\"n\":1}}{}", " ".repeat(MAX_INPUT));
    assert!(submit(&gateway, "reusable", &oversized).is_err());
    assert!(submit(&gateway, "../escape", "{\"n\":1}").is_err());
    assert!(gateway
        .submit("reusable", "review", "{\"n\":1}", "stale")
        .is_err());
    assert!(gateway
        .submit(
            "reusable",
            "unpublished",
            "{\"n\":1}",
            gateway.describe()["descriptor_digest"].as_str().unwrap()
        )
        .is_err());
    assert!(fixture.calls("run").is_empty());
    submit(&gateway, "reusable", "{\"n\":1}").unwrap();
    drain(&gateway).await;
    assert_eq!(fixture.calls("run").len(), 1);
}

#[tokio::test]
async fn equivalent_requests_are_observations_and_conflicting_identity_is_rejected_after_restart() {
    let fixture = Fixture::new();
    let gateway = fixture.open().await;
    submit(&gateway, "same", "{\"n\":1}").unwrap();
    submit(&gateway, "same", " { \"n\" : 1 } ").unwrap();
    assert!(submit(&gateway, "same", "{\"n\":2}").is_err());
    drain(&gateway).await;
    let expected = gateway.status("same").unwrap();
    drop(gateway);
    let reopened = fixture.open().await;
    assert_eq!(submit(&reopened, "same", "{\"n\":1}").unwrap(), expected);
    assert!(submit(&reopened, "same", "{\"n\":2}").is_err());
    drain(&reopened).await;
    assert_eq!(fixture.calls("run").len(), 1);
}

#[tokio::test]
async fn uncertain_outputs_retain_identity_and_consume_unresolved_capacity() {
    for mode in [
        "nonzero",
        "oversized",
        "duplicate-output",
        "fractional-output",
        "invalid-output",
        "wrong-run",
    ] {
        let mut fixture = Fixture::new();
        fixture.config.max_active = 1;
        fixture.mode(mode);
        let gateway = fixture.open().await;
        submit(&gateway, "uncertain", "{\"n\":1}").unwrap();
        drain(&gateway).await;
        let task = gateway.status("uncertain").unwrap();
        assert_eq!(task["state"], "unknown", "mode: {mode}");
        assert!(task.get("output_json").is_none(), "mode: {mode}");
        assert_eq!(submit(&gateway, "uncertain", "{\"n\":1}").unwrap(), task);
        assert!(
            submit(&gateway, "replacement", "{\"n\":1}").is_err(),
            "mode: {mode}"
        );
        assert_eq!(fixture.calls("run").len(), 1, "mode: {mode}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_before_executor_runs_prevents_host_dispatch() {
    let fixture = Fixture::new();
    let gateway = fixture.open().await;
    submit(&gateway, "cancel-before", "{\"n\":1}").unwrap();
    let task = gateway.cancel("cancel-before").await.unwrap();
    assert_eq!(task["state"], "cancelled");
    assert_eq!(task["cancel_requested"], true);
    drain(&gateway).await;
    assert!(fixture.calls("run").is_empty());
    assert!(fixture.calls("cancel").is_empty());
    assert_eq!(
        submit(&gateway, "cancel-before", "{\"n\":1}").unwrap(),
        task
    );
}

#[tokio::test]
async fn cancellation_retries_until_host_state_exists_without_claiming_rollback() {
    let fixture = Fixture::new();
    fixture.mode("delayed-start");
    let gateway = fixture.open().await;
    submit(&gateway, "startup-race", "{\"n\":1}").unwrap();
    wait_until(|| !fixture.calls("run").is_empty()).await;
    gateway.cancel("startup-race").await.unwrap();
    drain(&gateway).await;
    let task = gateway.status("startup-race").unwrap();
    assert_eq!(task["cancel_requested"], true);
    assert_eq!(task["state"], "unknown");
    assert!(fixture.root.join("cancel-startup-race").exists());
    assert!(fixture.calls("cancel").len() >= 2);
    assert_eq!(fixture.calls("run").len(), 1);
}

#[tokio::test]
async fn slow_cancellation_does_not_stop_processing_a_completed_host_response() {
    let fixture = Fixture::new();
    fixture.mode("slow-cancel");
    let gateway = fixture.open().await;
    submit(&gateway, "slow-cancel", "{\"n\":1}").unwrap();
    wait_until(|| fixture.root.join("started-slow-cancel").exists()).await;
    let caller = gateway.clone();
    let cancellation = tokio::spawn(async move { caller.cancel("slow-cancel").await });
    tokio::time::timeout(Duration::from_secs(1), async {
        while gateway.status("slow-cancel").unwrap()["state"] != "completed" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a slow cancellation blocked a completed host response");
    assert_eq!(
        gateway.status("slow-cancel").unwrap()["cancel_requested"],
        true
    );
    assert_eq!(fixture.calls("run").len(), 1);
    cancellation.await.unwrap().unwrap();
    drain(&gateway).await;
}

#[test]
fn process_shutdown_marks_inflight_work_unknown_and_never_redispatches() {
    let mut fixture = Fixture::new();
    fixture.config.max_active = 1;
    fixture.mode("wait");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let gateway = runtime.block_on(async {
        let gateway = fixture.open().await;
        submit(&gateway, "interrupted", "{\"n\":1}").unwrap();
        wait_until(|| fixture.root.join("started-interrupted").exists()).await;
        gateway
    });
    drop(runtime);
    drop(gateway);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let gateway = fixture.open().await;
        assert_eq!(gateway.status("interrupted").unwrap()["state"], "unknown");
        assert_eq!(
            submit(&gateway, "interrupted", "{\"n\":1}").unwrap()["state"],
            "unknown"
        );
        assert!(submit(&gateway, "replacement", "{\"n\":1}").is_err());
        drain(&gateway).await;
    });
    assert_eq!(fixture.calls("run").len(), 1);
}

#[tokio::test]
async fn observation_requires_known_identity_and_both_disclosure_grants() {
    let mut fixture = Fixture::new();
    let gateway = fixture.open().await;
    assert!(gateway.observe("foreign", false).await.is_err());
    submit(&gateway, "known", "{\"n\":1}").unwrap();
    drain(&gateway).await;
    assert!(gateway.observe("known", true).await.is_err());
    assert!(fixture.calls("observe").is_empty());
    let observation = gateway.observe("known", false).await.unwrap();
    let detail: Value =
        serde_json::from_str(observation["observation_json"].as_str().unwrap()).unwrap();
    assert_eq!(detail["include_payloads"], false);
    assert_eq!(detail["timestamp"], json!(0.25));
    drop(gateway);
    fixture.config.allow_payloads = true;
    fixture.config.state = fixture.root.join("permitted-state");
    let gateway = fixture.open().await;
    submit(&gateway, "known", "{\"n\":1}").unwrap();
    drain(&gateway).await;
    let observation = gateway.observe("known", true).await.unwrap();
    let detail: Value =
        serde_json::from_str(observation["observation_json"].as_str().unwrap()).unwrap();
    assert_eq!(detail["include_payloads"], true);
}

#[tokio::test]
async fn state_has_one_owner_and_rejects_config_or_deployment_rebinding() {
    let fixture = Fixture::new();
    let gateway = fixture.open().await;
    assert!(Gateway::open(fixture.config.clone()).await.is_err());
    drop(gateway);
    let mut changed = fixture.config.clone();
    changed.max_tasks += 1;
    assert!(Gateway::open(changed).await.is_err());
    let bytes = fs::read(&fixture.config.bundle).unwrap();
    fs::write(&fixture.config.bundle, [bytes.as_slice(), b" "].concat()).unwrap();
    assert!(Gateway::open(fixture.config.clone()).await.is_err());
    fs::write(&fixture.config.bundle, bytes).unwrap();
    fs::write(
        &fixture.config.host,
        format!("{FAKE_HOST}\n# changed deployment\n"),
    )
    .unwrap();
    assert!(Gateway::open(fixture.config.clone()).await.is_err());
}

#[tokio::test]
async fn live_host_or_bundle_mutation_fails_before_dispatch_and_is_retained_as_unknown() {
    for mutate_host in [false, true] {
        let fixture = Fixture::new();
        let gateway = fixture.open().await;
        let path = if mutate_host {
            &fixture.config.host
        } else {
            &fixture.config.bundle
        };
        let mut bytes = fs::read(path).unwrap();
        bytes.push(b' ');
        fs::write(path, bytes).unwrap();
        submit(&gateway, "changed", "{\"n\":1}").unwrap();
        drain(&gateway).await;
        assert_eq!(gateway.status("changed").unwrap()["state"], "unknown");
        assert!(fixture.calls("run").is_empty());
    }
}

#[tokio::test]
async fn only_explicit_valid_contracts_with_bidirectional_client_routes_are_exported() {
    for mode in ["route", "schema", "actor"] {
        let fixture = Fixture::new();
        match mode {
            "route" => {
                let mut bundle: Value =
                    serde_json::from_slice(&fs::read(&fixture.config.bundle).unwrap()).unwrap();
                bundle["topology"]["edges"] = json!([["client", "checker"]]);
                fs::write(&fixture.config.bundle, bundle.to_string()).unwrap();
            }
            _ => {
                let path = fixture.root.join("descriptor.json");
                let mut descriptor: Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                if mode == "schema" {
                    descriptor["agents"][0]["input_schema"]["unsupported"] = json!(true);
                } else {
                    descriptor["agents"][0]["name"] = json!("other");
                }
                fs::write(path, descriptor.to_string()).unwrap();
            }
        }
        assert!(
            Gateway::open(fixture.config.clone()).await.is_err(),
            "mode: {mode}"
        );
        assert!(fixture.calls("run").is_empty());
    }
}

#[test]
fn configuration_rejects_duplicate_keys_and_resolves_paths_against_its_file() {
    let fixture = Fixture::new();
    let path = fixture.root.join("mas-mcp.json");
    let mut config = serde_json::to_value(&fixture.config).unwrap();
    config["host"] = json!("host.py");
    config["bundle"] = json!("bundle.json");
    config["state"] = json!("private-state");
    fs::write(&path, config.to_string()).unwrap();
    let read = Config::read(&path).unwrap();
    assert_eq!(read.host, fixture.config.host);
    assert_eq!(read.bundle, fixture.config.bundle);
    assert_eq!(read.state, fixture.root.join("private-state"));
    let duplicate = config
        .to_string()
        .replacen('{', "{\"version\":\"mas-mcp/1\",", 1);
    fs::write(path, duplicate).unwrap();
    assert!(Config::read(Path::new(&fixture.root.join("mas-mcp.json"))).is_err());
}
