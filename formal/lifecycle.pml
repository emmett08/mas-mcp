/*
 * MCP-to-host lifecycle, two durable request identities.
 * Atomic blocks represent one journal transaction under the service lock.
 * Working admission and actual subprocess spawn are separate transitions.
 * One crash/restart may lose any unretained host response. External work may
 * outlive that crash. This is a safety model, with no fairness/liveness claim.
 */
#ifndef MAX_ACTIVE
#define MAX_ACTIVE 1
#endif
#ifndef MAX_TASKS
#define MAX_TASKS 2
#endif
#define N 2
#define ATTEMPTS 3

mtype = { Absent, Submitted, Working, Completed, Unknown, Cancelled };
mtype state[N];
byte content[N];
byte frozen[N];
byte attempts[N];
byte dispatches[N];
bool cancel_requested[N];
bool cancellation_tried[N];
bool unknown_seen[N];
bool spawned[N];
byte active_count;
byte accepted_count;
bool restarted;
bool duplicate_observed;
bool changed_rejected;
bool capacity_rejected;
bool lifetime_rejected;
bool completed_after_cancel;

inline invariants() {
    assert(active_count <= MAX_ACTIVE);
    assert(accepted_count <= MAX_TASKS);
    assert(active_count == ((state[0] == Submitted || state[0] == Working || state[0] == Unknown) +
                            (state[1] == Submitted || state[1] == Working || state[1] == Unknown)));
    assert(accepted_count == ((state[0] != Absent) + (state[1] != Absent)));
    assert(dispatches[0] <= 1 && dispatches[1] <= 1);
    assert(content[0] == frozen[0] && content[1] == frozen[1]);
    assert(state[0] != Cancelled || dispatches[0] == 0);
    assert(state[1] != Cancelled || dispatches[1] == 0);
    assert(state[0] != Completed || dispatches[0] == 1);
    assert(state[1] != Completed || dispatches[1] == 1);
#ifdef WITNESS_COMPLETE
    assert(state[0] != Completed && state[1] != Completed);
#endif
#ifdef WITNESS_REPLAY
    assert(!duplicate_observed);
#endif
#ifdef WITNESS_CANCELLED
    assert(state[0] != Cancelled && state[1] != Cancelled);
#endif
#ifdef WITNESS_UNKNOWN_BEFORE_SPAWN
    assert(!((state[0] == Unknown && dispatches[0] == 0) ||
             (state[1] == Unknown && dispatches[1] == 0)));
#endif
#ifdef WITNESS_UNKNOWN_AFTER_SPAWN
    assert(!((state[0] == Unknown && dispatches[0] == 1) ||
             (state[1] == Unknown && dispatches[1] == 1)));
#endif
#ifdef WITNESS_COMPLETION_AFTER_CANCEL
    assert(!completed_after_cancel);
#endif
#ifdef WITNESS_CAPACITY
    assert(!capacity_rejected);
#endif
#ifdef WITNESS_LIFETIME
    assert(!lifetime_rejected);
#endif
#ifdef WITNESS_CHANGED
    assert(!changed_rejected);
#endif
}

inline submit(i, proposed) {
    attempts[i]++;
    if
    :: state[i] == Absent ->
        if
        :: accepted_count >= MAX_TASKS -> lifetime_rejected = true
        :: accepted_count < MAX_TASKS ->
            if
#ifdef MUT_CAPACITY
            :: true ->
#else
            :: active_count < MAX_ACTIVE ->
#endif
                content[i] = proposed;
                frozen[i] = proposed;
                state[i] = Submitted;
                active_count++;
                accepted_count++
            :: active_count >= MAX_ACTIVE -> capacity_rejected = true
            fi
        fi
    :: state[i] != Absent ->
        if
        :: content[i] != proposed ->
#ifdef MUT_REBIND
            content[i] = proposed;
#endif
            changed_rejected = true
        :: content[i] == proposed ->
            duplicate_observed = true;
#ifdef MUT_COMPLETED_REPLAY
            if
            :: state[i] == Completed && active_count < MAX_ACTIVE ->
                state[i] = Submitted;
                spawned[i] = false;
                active_count++
            :: else -> skip
            fi;
#endif
#ifdef MUT_UNKNOWN_RETRY
            if
            :: state[i] == Unknown ->
                state[i] = Submitted;
                spawned[i] = false
            :: else -> skip
            fi
#endif
            skip
        fi
    fi
}

inline cancel_request(i) {
    cancellation_tried[i] = true;
    if
    :: state[i] == Absent -> skip
    :: state[i] != Absent ->
        cancel_requested[i] = true;
        if
        :: state[i] == Submitted -> state[i] = Cancelled; active_count--
#ifdef MUT_CANCEL_CERTAINTY
        :: state[i] == Working && spawned[i] ->
            state[i] = Cancelled; active_count--
#endif
        :: else -> skip
        fi
    fi
}

inline become_unknown(i) {
    if
    :: state[i] == Submitted || state[i] == Working ->
        state[i] = Unknown;
        unknown_seen[i] = true
    :: else -> skip
    fi
}

init {
    byte i;
    byte proposed;
    state[0] = Absent;
    state[1] = Absent;
    do
    :: atomic {
        (attempts[0] < ATTEMPTS || attempts[1] < ATTEMPTS);
        if
        :: attempts[0] < ATTEMPTS -> i = 0
        :: attempts[1] < ATTEMPTS -> i = 1
        fi;
        if :: proposed = 1 :: proposed = 2 fi;
        submit(i, proposed);
        invariants()
    }
    :: atomic {
        (state[0] == Submitted || state[1] == Submitted);
        if
        :: state[0] == Submitted -> i = 0
        :: state[1] == Submitted -> i = 1
        fi;
        assert(!cancel_requested[i]);
        state[i] = Working;
        invariants()
    }
    :: atomic {
        ((state[0] == Working && !spawned[0]) ||
         (state[1] == Working && !spawned[1]));
        if
        :: state[0] == Working && !spawned[0] -> i = 0
        :: state[1] == Working && !spawned[1] -> i = 1
        fi;
        assert(!unknown_seen[i]);
        dispatches[i]++;
        spawned[i] = true;
        invariants()
    }
    :: atomic {
        ((state[0] == Working && spawned[0]) ||
         (state[1] == Working && spawned[1]));
        if
        :: state[0] == Working && spawned[0] -> i = 0
        :: state[1] == Working && spawned[1] -> i = 1
        fi;
        if
        :: state[i] = Completed;
           if :: cancel_requested[i] -> completed_after_cancel = true :: else -> skip fi;
           active_count--
        :: become_unknown(i)
        fi;
        invariants()
    }
    :: atomic {
        (!cancellation_tried[0] || !cancellation_tried[1]);
        if
        :: !cancellation_tried[0] -> i = 0
        :: !cancellation_tried[1] -> i = 1
        fi;
        cancel_request(i);
        invariants()
    }
    :: atomic {
        !restarted;
        restarted = true;
        become_unknown(0);
        become_unknown(1);
        invariants()
    }
    :: break
    od;
    invariants()
}
