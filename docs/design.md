# MAS operations through MCP

`mas-mcp` lets an MCP client invoke explicitly published operations from a
prebuilt MAS application. The Rust host continues to execute that application,
while Nehemiah continues to admit its messages, enforce routes and budgets, and
retain task and effect records. The MCP server adds discovery, durable client
request identities and a bounded local service boundary. It adds no Nehemiah
message kind, worker frame or host execution mode.

## Existing contracts

The design was checked against these repository revisions:

| Source | Revision | Relevant contract |
| --- | --- | --- |
| `emmett08/mas-host` | `6508042ea86d26bb35106d919a8699d5f1a2e782` | `describe`, `run`, `observe` and `cancel` CLI commands |
| `emmett08/mas` | `7bbd1301ed651e0994390ac04b5c340a700fdfcd` | Nehemiah codec, protocol store and read-only observation |
| Protocol consumed by that host | `fe644bbf3acb3ccc2abaffd54bcc4b367053fe4b` | Host's existing Cargo dependency pin |

The runnable example pins its reviewed host and MCP-server revisions separately.
The host's protocol dependency determines its actual execution semantics.
Reading a newer protocol checkout does not upgrade that dependency.

`mas-host describe --bundle FILE` validates executable bindings and projects
`mas-host-descriptor/1`: actor names, operation names, object schemas, bundle
digest and numeric/message limits. Paths, environment grants, prompts and
provider settings remain private. This descriptor supplies contracts; the MCP
configuration supplies publication authority.

`mas-host run --bundle FILE --agent NAME --input - --state DIR --run ID`
uses the existing immutable bundle. Native, command, LLM and workflow actors
therefore retain their usual admission, execution and recovery behaviour.
Only a successful process exit with the expected host result can establish
service completion. An error after dispatch retains uncertainty about external
effects.

`mas-host observe` opens both host and protocol databases read-only. It takes no
execution-owner lock and performs no reconciliation, delivery or acknowledgement.
Its two database snapshots can reflect adjacent commits. By comparison,
`inspect` opens normal stores and may reconcile state, so observation is the
appropriate polling interface.

## Integration choice

| Option | Consequence | Decision |
| --- | --- | --- |
| Invoke the pinned host CLI | Reuses its execution identity, process guardian, bundle validation and administrative contracts | Selected for this local profile |
| Link `mas-host-core::Engine` | Requires the MCP executable to implement the hidden `__supervise` launcher and become the host execution identity | Suitable for a future deliberately shared runtime package |
| Forward MCP calls to the A2A service | Adds HTTP authentication, A2A negotiation and another durable task journal | Useful for remote deployment, unnecessary for one local operator |
| Call MAS gateway or host IPC directly | Exposes lower-level protocol authority without supplying native host execution or recovery | Does not provide the required application service |

The embedding constraint is explicit in `mas-host-worker/src/lib.rs`: workers
launch `current_exe()` with `__supervise`. CLI composition keeps that responsibility
with the host executable that already implements it. The server never constructs
shell command strings and never accepts an executable or path from tool input.

## SDK and module boundaries

Stdio is the transport. The official `rmcp` SDK implements MCP framing, negotiation,
requests and responses. Version 3.5.1 is pinned with Cargo.lock. The supported
2025-11-25 initialisation path is exercised by an independent official JavaScript
client and binary-level tests. The SDK's length-limited codec is composed with
Tokio framed streams to bound input before allocation can grow without limit.

`Dicklesworthstone/fastmcp_rust` was assessed at
`02b7dfc907682cd866a8059f13082037dfdde5f4`. That checkout selects
`nightly-2026-08-25` and an asupersync-only runtime, with Tokio adapters explicitly
unsupported. Replacing the SDK would require a runtime and toolchain migration
without an identified benefit for the five-tool local service. The official
SDK remains the selected implementation. This is a fit judgement for this
application, not a general framework ranking.

| Module | Responsibility and design choice |
| --- | --- |
| `transport` | Bound SDK stdio framing and own the MCP session |
| `server` | Adapt MCP arguments and results to the application facade |
| `gateway` | Compose the application and enforce observation disclosure |
| `catalog` | Publish approved contracts and validate exact application values |
| `execution` | Coordinate admission, one host dispatch, cancellation and completion |
| `host` | Adapt the fixed CLI contract with bounded process I/O |
| `journal` | Own SQLite identity, reservation and state transitions |
| `config` | Parse operator-owned deployment settings |

The adapter and facade patterns address actual external contract differences.
The journal expresses the small lifecycle as guarded SQL transitions rather than
an object hierarchy. RAII owns the state-directory lock and releases in-flight
accounting on normal completion, cancellation or panic. The execution coordinator
polls host completion concurrently with cancellation forwarding, so a slow
cancellation subprocess cannot stop draining the host result.

This applies single responsibility and interface segregation through cohesive
modules. Gateway users depend on the narrow application facade rather than MCP
or CLI details. Concrete structs preserve explicit dependencies and avoid a
trait hierarchy for backends that do not exist. A second genuinely substitutable
host or persistence implementation would justify a contract trait with the same
identity, uncertainty and capacity tests.

The initial large startup/publication method was **EXTRACTED** into `Catalog`.
The execution lifecycle was **EXTRACTED** into `Execution`. The CLI invocation
helper is **RETAINED** within `Host` because its callers share the same fixed
process ownership and bounded I/O contract. No generic repository, singleton,
service locator or event bus is introduced. The dispatch match in `server`
selects five distinct tool operations; it does not contain alternate backends or
algorithms.

## Request and authority boundary

The MCP connection uses stdio. The process owner supplies the host executable,
immutable bundle, private state directory and exported operations. An MCP tool
call selects a configured export and supplies application input and a stable
request identity. The server checks the operation contract before recording a
new request. It never builds code or changes the bundle during a call.

Each published operation must have explicit input and output object schemas and
both host-ingress request and reply routes. Publication does not grant direct
access to internal actors. The application topology remains authoritative for
the delegates its exported actor can invoke.

The service has one local filesystem authority and one owner lock. It is not a
multi-tenant network service. The stdio client's authority comes from its ability
to launch the configured process and access the same local account. External
workers retain the host's trusted-code deployment assumption and operator
permissions. The MCP boundary provides neither a process sandbox nor additional
Kubernetes or GitHub privileges.

The service publishes only configured operation descriptions and schemas.
Observation payloads require both a configuration grant and an explicit call
option. The default observation omits application inputs, outputs and evidence.
Final outputs are available to the client that owns this local service; this
operation-result disclosure is distinct from exposing every internal message.
Application authors remain responsible for secrets in public descriptions,
identifiers and returned output.

## Durable execution identity

The private SQLite journal binds the service to its configuration, MCP server executable, host executable
and bundle. Startup rejects a different identity against retained state. The
initial profile has no migration or remote effect-resolution operation. Upgrades
and reconciliation must preserve outstanding obligations and their original
records.

The journal records each accepted request identity together with its selected
export and canonical exact input before dispatch. The unique request identity
and complete stored content determine duplicate handling:

- The same identity and content returns the existing task, including its retained
  completion or uncertainty. This does not dispatch another host.
- The same identity with different content rejects.
- A new identity requires available lifetime and active-capacity reservations.

Request identity is separate from the MCP JSON-RPC message ID. Reconnecting or
repeating a tool call cannot create execution permission for an already retained
request. A new identity denotes new work and does not establish that repeating
an earlier external action is safe.

## Lifecycle and cancellation

| State | Meaning | Permitted subsequent state |
| --- | --- | --- |
| `submitted` | Request and capacity reservation are durable | `working`, `cancelled`, or `unknown` on restart |
| `working` | Dispatch has been admitted; subprocess creation or execution may be in progress | `completed` or `unknown` |
| `completed` | A valid successful host result has been retained | Retained unchanged |
| `cancelled` | Cancellation won before dispatch admission | Retained unchanged; no subprocess dispatch |
| `unknown` | An admitted request has no retained confirmed completion | Retained unchanged; no automatic dispatch |

Admission to `working`, process creation, process completion and durable result
retention are separate events. A process crash between any two can lose evidence
of what occurred. On restart, every unfinished `submitted` or `working` record
becomes `unknown`, even if the host might not have started. This conservative
choice avoids converting an absent response into permission to repeat work.

Cancellation always retains an intent flag. Before dispatch admission, a
transaction can establish `cancelled` and guarantee that the host will not be
spawned for that request. After admission, the server forwards intent to
`mas-host cancel`; it cannot establish that remote providers or external effects
stopped. The host may not yet have created its run journal when cancellation is
first requested, so intent can remain pending until forwarding succeeds. A
confirmed host success may subsequently produce `completed` with cancellation
intent still recorded.

Clean input EOF drains accepted work. Abrupt loss of the MCP process is a
different event: the host or external actions may survive it. Restart preserves
their uncertainty. Unknown requests retain active-capacity reservations along
with submitted and working requests, preventing an interrupted service from
freeing capacity merely because its response was lost. Reconciliation is an
explicit deployment responsibility; this profile provides no automatic reset.

`max_active` bounds the service's reserved unfinished work. `max_tasks` bounds
lifetime accepted identities, including terminal records. Duplicate requests
consume neither another reservation nor another lifetime identity. Nehemiah's
message, byte, effect and wall-clock budgets remain additional per-run bounds.

## Numeric, transport and error contracts

Application values use the host's exact-integer JSON profile. Duplicate object
members, fractional or exponent-form numbers, invalid Unicode and excessive
size or depth reject before application admission. Strings or scaled integers
can represent decimal quantities under an explicit application schema.
`mas_submit` carries the encoded object in `input_json`, and a completed task
returns `output_json`. These JSON-text strings preserve exact large integers
across MCP clients whose native numeric type would otherwise round them. The
server parses and validates the encoded object before admitting it.

Observation metadata has a separate representation: the existing host includes
fractional Unix seconds in `run.deadline` and task `closed_at`. Those fields are
preserved as observation data. They must not be passed through the application's
integer-only parser or silently rounded. `mas_observe` returns the host snapshot
as `observation_json` so its encoding survives client transport unchanged.

Every transport request, child-process output and response is bounded. Oversized
or malformed host output cannot establish completion and cannot authorise a
retry. stdout carries MCP JSON-RPC messages only. Diagnostics use stderr and
must not disclose retained application inputs or credentials. Tool failures and
protocol-level request errors remain distinct.

MCP request cancellation and the application `mas_cancel` operation have different
identities. Ending a tool wait does not erase a durable MAS request. Clients use
its returned request identity to inspect its retained status.

## Example and verification

The example in `mas-host-examples` exercises the direction
MCP client → MAS MCP server → Rust host → Nehemiah application actors.
The existing host MCP-tool guide documents the other direction, where an actor
calls an external MCP service. Both can coexist under explicitly configured
delegates, but the example needs only the first direction to establish this
server's integration contract.

The finite lifecycle model and its implementation correspondence are documented
in [the formal checks](../formal/README.md). Implementation tests supply separate
evidence for actual JSON framing, SQLite transactions, process invocation,
schema validation, observation and replay. A deterministic real-host application
establishes the integration without an API key. A configured LLM application
uses the same boundary, with model quality assessed separately from protocol
conformance.
