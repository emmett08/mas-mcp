# MCP request lifecycle checks

Run from the repository root:

```sh
sh scripts/verify-model.sh
```

SPIN, a C compiler and Python 3 are required. `SPIN=/path/to/spin` and
`CC=/path/to/cc` select tools. `--out DIRECTORY` changes the evidence destination.
The harness records tool versions, the model SHA-256, exact commands, search
output, unreached states and all expected counterexample trails and replays.
Generated files remain under `formal/evidence/` by default and are not committed.

## Service and finite environment

`lifecycle.pml` represents two durable request identities, two abstract complete
input values, at most three submit/retry attempts per identity, one cancellation
attempt per identity and at most one crash/restart. The harness checks active
and lifetime capacities `(1,1)`, `(1,2)` and `(2,2)`, both with and without
partial-order reduction.

Each transition represents one serialised service event. Nondeterministic choices
enumerate interleavings of submissions, dispatch admission, subprocess creation,
host completion, cancellation and restart. An atomic block corresponds to one
journal transaction or one externally visible event. Working admission and actual
process spawn are deliberately separate, as are host dispatch and retained
completion. A crash can therefore occur before spawn, during execution or before
an observed result becomes durable. The success transition abstracts successful
host exit, output validation and result retention as a single retained event.
Loss before that commit is represented by crash/restart or an unknown outcome.

SQLite commits are assumed durable and atomic under one process owner lock.
Payload classes represent equality and inequality of canonical complete content,
including the export. Actual JSON parsing, schema checking and executable hashing
are implementation-test obligations. Hash collisions, storage corruption,
malicious local workers, filesystem races and multi-tenant access are excluded.

Unknown requests retain their capacity reservation. A late completion from a
previous server lifetime cannot settle an unknown record in this profile. A
duplicate request only observes retained state. The model has no effect-resolution
transition, state deletion, identity reuse or automatic retry.

The environment can stop at any point, and external work can remain unfinished.
This is intentional: the model makes **no liveness, elapsed-time or eventual
successful-completion claim**. Valid finite termination includes unfinished
work. The one-crash bound permits checking recovery safety without assuming that
a stopped provider eventually returns. No fairness option is used.

## Properties and implementation correspondence

| Property | Model assertion or transition | Implementation obligation |
| --- | --- | --- |
| One host dispatch per accepted identity | Each `dispatches[i] <= 1` | Duplicate lookup returns the retained record, and recovery never schedules it again |
| Immutable request content | `content[i] == frozen[i]` | Unique request ID plus comparison of complete canonical export/input content |
| Uncertainty grants no dispatch | `!unknown_seen[i]` at subprocess creation | Restart/error records stay `unknown`, including on exact retries |
| Confirmed cancellation precedes dispatch | `Cancelled` implies zero dispatches | Cancellation and dispatch admission are ordered by the same journal lock/transaction |
| Completion has a dispatch | `Completed` implies exactly one dispatch | Completion requires the expected successful child-process result |
| Capacity is reserved | Counter equals the number of submitted, working and unknown records and stays within `MAX_ACTIVE` | Admission checks and reserves capacity before execution; unknown records continue to count |
| Lifetime IDs remain retained | Accepted counter equals non-absent identities and stays within `MAX_TASKS` | Terminal requests are not deleted or made eligible as new identities |
| Cancellation intent permits a later confirmed success | Completion after cancellation is reachable | Forwarded cancellation remains intent; successful host completion can be retained with the flag |

The mapping identifies transaction and lifecycle boundaries rather than proving
that the Rust implementation refines the model. Actual process supervision,
numeric parsing, redaction, bounded I/O and host behaviour need their own tests.
Nehemiah retains its separate message, task, effect and budget checks unchanged.

The corresponding functions are `Journal::submit`, `begin`, `finish`, `cancel`
and `open` in `src/journal.rs`, and `Execution::submit`/`execute` in
`src/execution.rs`. Journal unit tests exercise the restart reservation, unchanged
request identity, single dispatch admission, cancellation-before-dispatch,
success-after-intent and deployment ownership boundaries. These implementation
checks complement the finite model rather than inheriting its state counts.

## Non-vacuity and deliberate defects

Five isolated compile-time mutations must produce assertion violations:

- Retry an unknown request by returning it to submitted state.
- Dispatch a completed request again on an exact duplicate submission.
- Replace the content retained under an existing identity.
- Classify cancellation after actual dispatch as confirmed cancellation.
- Admit a new request despite exhausted active capacity.

Nine inverted assertions witness reachable completion, exact duplicate lookup,
confirmed early cancellation, uncertainty before and after spawn, completion
after cancellation intent, active-capacity rejection, lifetime-capacity rejection
and changed-content rejection. A counterexample in one of these witness runs is
expected evidence that the named state is reachable, rather than a defect in the
positive model.

Every expected counterexample is replayed using the same preprocessor defines.
Positive safety runs check assertions, array bounds and invalid end states using
full-state storage. There is no bitstate or hash-compact approximation. Search
depth is 100,000, verifier memory is capped at 256 MiB, and each generation,
compilation, search or replay command has a 30-second limit. A timeout, memory
limit or depth exhaustion fails the harness as incomplete evidence.

These commands follow the official SPIN documentation for
[verifier options](https://spinroot.com/spin/Man/Pan.html) and
[atomic transitions](https://spinroot.com/spin/Man/atomic.html). The evidence
records the installed version. A successful run establishes the stated safety
properties for this finite abstraction and its assumptions. It establishes
neither exactly-once external effects nor unrestricted implementation correctness.
