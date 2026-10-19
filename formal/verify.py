#!/usr/bin/env python3
"""Exhaustive finite safety checks, mutation detection and reachability witnesses."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=Path(__file__).parent / "evidence")
    args = parser.parse_args()
    spin = shutil.which(os.environ.get("SPIN", "spin"))
    cc = shutil.which(os.environ.get("CC", "cc"))
    if not spin or not cc:
        sys.exit("SPIN and a C compiler are required; set SPIN or CC to an executable path")
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=True)
    source = Path(__file__).with_name("lifecycle.pml")
    summary = {
        "version": "mas-mcp-model-evidence/1",
        "model_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
        "spin": subprocess.check_output([spin, "-V"], text=True).strip(),
        "compiler": subprocess.check_output([cc, "--version"], text=True).splitlines()[0],
        "bounds": {"identities": 2, "payload_classes": 2, "attempts_per_identity": 3,
                   "cancellations_per_identity": 1, "restarts": 1},
        "claims": "Finite safety and reachability only; no liveness or implementation refinement proof",
        "checks": [],
    }
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")

    def execute(argv, directory, name):
        with (directory / (name + ".log")).open("w") as log:
            log.write("$ " + " ".join(argv) + "\n")
            log.flush()
            try:
                process = subprocess.run(argv, cwd=directory, stdout=log,
                                         stderr=subprocess.STDOUT, timeout=30)
            except subprocess.TimeoutExpired:
                log.write("\nINCOMPLETE: 30-second command deadline exceeded\n")
                raise RuntimeError(f"{directory.name}: {name} timed out")
        return process.returncode, (directory / (name + ".log")).read_text()

    def check(name, defines=(), no_reduce=False, counterexample=False, expected_assertion=None):
        directory = out / name
        if directory.exists():
            # Each run is independent: no stale pan executable or trail is usable.
            shutil.rmtree(directory)
        directory.mkdir()
        shutil.copyfile(source, directory / "lifecycle.pml")
        flags = ["-D" + define for define in defines]
        code, _ = execute([spin, *flags, "-a", "lifecycle.pml"], directory, "generate")
        if code:
            raise RuntimeError(f"{name}: verifier generation failed")
        compile_args = [cc, "-O2", "-DSAFETY", "-DNOCLAIM", "-DMEMLIM=256"]
        if no_reduce:
            compile_args.append("-DNOREDUCE")
        code, _ = execute([*compile_args, "-o", "pan", "pan.c"], directory, "compile")
        if code:
            raise RuntimeError(f"{name}: verifier compilation failed")
        code, log = execute(["./pan", "-b", "-m100000", "-w20"], directory, "search")
        errors = re.search(r"errors:\s*(\d+)", log)
        if not errors or re.search(r"max search depth too small|out of memory|VECTORSZ is too small", log):
            raise RuntimeError(f"{name}: incomplete search")
        count = int(errors.group(1))
        if counterexample:
            if count != 1 or "assertion violated" not in log:
                raise RuntimeError(f"{name}: expected assertion counterexample absent")
            if expected_assertion and expected_assertion not in log:
                raise RuntimeError(f"{name}: counterexample violates a different assertion")
            trails = list(directory.glob("*.trail"))
            if len(trails) != 1:
                raise RuntimeError(f"{name}: one replayable trail required")
            replay_code, replay = execute(
                [spin, *flags, "-t", "-k", trails[0].name, "-p", "-g", "-l", "lifecycle.pml"],
                directory, "replay")
            if replay_code or "assertion violated" not in replay:
                raise RuntimeError(f"{name}: counterexample replay failed")
        elif code or count:
            raise RuntimeError(f"{name}: safety violation; inspect {directory / 'search.log'}")
        states = re.search(r"([\d.e+]+)\s+states, stored", log)
        transitions = re.search(r"([\d.e+]+)\s+transitions", log)
        record = {"name": name, "defines": list(defines), "partial_order_reduction": not no_reduce,
                  "result": "expected_counterexample_replayed" if counterexample else "finite_safety_pass",
                  "errors": count, "stored_states": states.group(1) if states else None,
                  "transitions": transitions.group(1) if transitions else None}
        summary["checks"].append(record)
        (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(f"{name}: {record['result']} ({record['stored_states']} states)", flush=True)

    for active, tasks in [(1, 1), (1, 2), (2, 2)]:
        for reduction in [True, False]:
            check(f"safety-active{active}-tasks{tasks}-{'por' if reduction else 'full'}",
                  [f"MAX_ACTIVE={active}", f"MAX_TASKS={tasks}"], not reduction)
    mutations = {"UNKNOWN_RETRY": "!(unknown_seen[i])",
                 "COMPLETED_REPLAY": "dispatches[0]<=1",
                 "REBIND": "content[0]==frozen[0]",
                 "CANCEL_CERTAINTY": "dispatches[0]==0",
                 "CAPACITY": "active_count<=1"}
    for mutant, assertion in mutations.items():
        check("mutation-" + mutant.lower(), ["MUT_" + mutant], counterexample=True,
              expected_assertion=assertion)
    for witness in ["COMPLETE", "REPLAY", "CANCELLED", "UNKNOWN_BEFORE_SPAWN",
                    "UNKNOWN_AFTER_SPAWN", "COMPLETION_AFTER_CANCEL", "CAPACITY", "CHANGED"]:
        check("witness-" + witness.lower(), ["WITNESS_" + witness], counterexample=True)
    check("witness-lifetime", ["WITNESS_LIFETIME", "MAX_TASKS=1"], counterexample=True)
    summary["status"] = "passed"
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(f"Evidence: {out / 'summary.json'}")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
