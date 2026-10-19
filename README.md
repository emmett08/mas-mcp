# MAS MCP server

A Rust **stdio MCP server** that publishes approved operations from
[`mas-host`](https://github.com/emmett08/mas-host). The host executes the
application and [`Nehemiah`](https://github.com/emmett08/mas) admits its messages,
routes and effects. The server uses the official **`rmcp` 3.5.1** SDK.

An MCP client can submit a multi-agent workflow, inspect its retained result and
observe the actual host message flow. Configuration selects the executable,
immutable application bundle and published operations. Tool calls supply only
application data and request identities.

## Try the working example

The [release-review example](https://github.com/emmett08/mas-host-examples/tree/main/examples/mcp-review)
uses an official JavaScript MCP client, this server and the real Rust host. A
deterministic policy check and a separate fixture reviewer must both approve a
release. No provider API key is required.

From that repository on Linux:

```sh
make setup
make mcp-setup
make mcp
```

The example also generates a retained configuration for a desktop MCP client.
The server and host must run in Linux, matching the host's current deployment
profile. A Mac client needs a Linux execution environment for this application.

## Build and configure

Prerequisites: Linux, Git, Rust **1.99.0**, a C compiler, and an installed
`mas-host` with the runtimes required by your application. The toolchain,
dependency versions and Cargo lockfile are pinned.

```sh
cargo build --release --locked
target/release/mas-mcp --version
```

Build the application with the host first:

```sh
/absolute/path/mas-host build --project /absolute/path/application \
  --sdk-root /absolute/path/mas-host-checkout
```

Use the returned bundle path in `server.json`:

```json
{
  "version": "mas-mcp/1",
  "host": "/absolute/path/mas-host",
  "bundle": "/absolute/path/returned-bundle.json",
  "state": "/absolute/path/private-mcp-state",
  "exports": [
    {
      "name": "release_review",
      "agent": "review",
      "description": "Review a proposed release against the configured policy."
    }
  ],
  "max_active": 4,
  "max_tasks": 1000,
  "allow_payloads": false
}
```

Paths resolve relative to the configuration file. The server creates a private
state directory if absent. An existing directory must have mode `0700`.
Each export needs explicit input/output object schemas and both client request
and reply routes in the bundle. Preserve the generated bundle and all its
content-addressed workers.

Register this command with a client that supports MCP stdio:

```json
{
  "mcpServers": {
    "mas": {
      "command": "/absolute/path/mas-mcp",
      "args": ["--config", "/absolute/path/server.json"]
    }
  }
}
```

One server process owns each state directory. It runs with the local operator's
permissions. This release supplies local stdio, with no HTTP listener or remote
authentication service.

## Tools and results

| Tool | Purpose |
| --- | --- |
| `mas_describe` | Return published operation contracts and their descriptor digest |
| `mas_submit` | Record a request identity and dispatch its approved operation once |
| `mas_status` | Read the retained task state and completed application output |
| `mas_observe` | Read committed host/protocol observations, with payloads omitted by default |
| `mas_cancel` | Persist cancellation intent and forward it to the host |

First call `mas_describe`, then submit an input matching its operation schema:

```json
{
  "request_id": "release-42",
  "operation": "release_review",
  "descriptor_digest": "COPY_FROM_MAS_DESCRIBE",
  "input_json": "{\"release\":\"release-42\",\"has_rollback\":true,\"critical_findings\":false}"
}
```

Poll `mas_status` with `{"task_id":"release-42"}`. Successful tool responses
contain matching JSON text and `structuredContent`. Application `input_json`
and `output_json` are JSON strings so clients preserve exact
integers up to Nehemiah's 1,024-digit bound. Fractional application numbers, duplicate keys and invalid schemas
are rejected. Observations use `observation_json`, which also preserves the
host's fractional timestamp metadata.

`completed` means the host finished and its output passed the published schema.
The output may contain a refusal or an unsuccessful application decision.
`cancelled` confirms cancellation before dispatch. After dispatch, cancellation
is intent and the task may still complete. `unknown` retains uncertainty about
effects and requires operator reconciliation.

Repeating an identical request ID and canonical input returns the same record.
Changed content under that ID rejects. Restart converts unfinished records to
`unknown` and never dispatches them again. Unknown requests retain their active
capacity reservation. `max_tasks` bounds all retained identities, including
completed ones. Client disconnection allows accepted work to finish within its
original host deadline. Abrupt server loss leaves unfinished records unresolved.

The state is bound to the server executable, host executable, bundle and
configuration. Keep these immutable for retained work. There is no automatic
state migration or remote effect resolution. Operators must reconcile unknown
effects before deciding whether replacement work is safe.

Input application JSON is limited to 32 KiB, incoming MCP frames to 256 KiB,
each captured host stream to 1 MiB and complete tool results to less than 1 MiB.
Completed application output is limited to 32 KiB. Oversized results retain
uncertainty rather than granting another dispatch. Payload observation requires
both `allow_payloads: true` and `include_payloads: true` in the call.

## Verification and structure

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
sh scripts/verify-model.sh    # requires Python 3, SPIN and a C compiler
```

The tests exercise exact JSON, schemas, immutable identities, durable replay,
restart, cancellation races, bounded stdio and disclosure. The example separately
checks official-client interoperability and real host delegation. SPIN checks
the documented finite lifecycle model, including deliberate defects. These
checks make no claim about live model quality or exactly-once external effects.

The [design](docs/design.md) explains the SDK and architecture choices. Transport,
MCP adaptation, operation contracts, execution, host CLI adaptation and journalling
have separate modules. [Formal checks](formal/README.md) state their assumptions
and implementation correspondence.

GPL-3.0-only, matching the host and protocol dependencies.
