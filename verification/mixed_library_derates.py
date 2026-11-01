#!/usr/bin/env python3
"""Bounded black-box timing differential, with caller-supplied pinned executables.

The JSON case file is a list of {name, libraries, netlist, sdc, endpoints}.
All inputs are read-only. Output workspaces must not already exist. Each case is
limited to three libraries, 32 endpoints and a 90-second tool invocation. Run
this script in the project's memory-limited, GNU-time-measured scope.
"""

import argparse
import hashlib
import json
import math
from pathlib import Path
import subprocess


def digest(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def tcl_word(value):
    value = str(value)
    if any(c in value for c in "{}\n\r"):
        raise ValueError("unsupported Tcl path characters")
    return "{" + value + "}"


def run(command, directory, label, *, json_output=False):
    result = subprocess.run(command, capture_output=True, timeout=90, check=False)
    (directory / (label + ".stdout")).write_bytes(result.stdout)
    (directory / (label + ".stderr")).write_bytes(result.stderr)
    if result.returncode:
        raise RuntimeError(f"{label}: exit {result.returncode}")
    if not json_output:
        return None
    output = json.loads(result.stdout)
    if not output.get("ok"):
        raise RuntimeError(f"{label}: tool rejected input: {output}")
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rusteda", required=True, type=Path)
    parser.add_argument("--ruststa", required=True, type=Path)
    parser.add_argument("--reference", required=True, type=Path)
    parser.add_argument("--cases", required=True, type=Path)
    parser.add_argument("--workspace", required=True, type=Path)
    parser.add_argument("--tolerance-ns", type=float, default=0.001)
    args = parser.parse_args()
    args.workspace = args.workspace.resolve()
    if not math.isfinite(args.tolerance_ns) or not 0 < args.tolerance_ns <= 0.001:
        raise ValueError("tolerance must be positive and at most one picosecond")
    if args.cases.stat().st_size > 1024 * 1024:
        raise ValueError("case metadata limit exceeded")
    cases = json.loads(args.cases.read_text())
    if not isinstance(cases, list) or not 1 <= len(cases) <= 32:
        raise ValueError("require one to 32 cases")
    args.workspace.mkdir(parents=True, exist_ok=False)
    evidence = {"status": "running", "tolerance_ns": args.tolerance_ns, "cases": [],
                "tools": {str(p.resolve()): digest(p) for p in (args.rusteda, args.ruststa, args.reference)}}
    output = args.workspace / "result.json"
    output.write_text(json.dumps(evidence, indent=2))
    try:
        for case in cases:
            name = case["name"]
            if not name.replace("-", "").replace("_", "").isalnum():
                raise ValueError("case name must be a shell-safe handle")
            libraries = [Path(p).resolve() for p in case["libraries"]]
            endpoints = case["endpoints"]
            if not 1 <= len(libraries) <= 3 or not 1 <= len(endpoints) <= 32:
                raise ValueError("case interface limit exceeded")
            directory = args.workspace / name
            directory.mkdir()
            workspace = directory / "native"
            inputs = libraries + [Path(case[k]).resolve() for k in ("netlist", "sdc")]
            for path in inputs:
                if path.stat().st_size > 64 * 1024 * 1024:
                    raise ValueError("input file limit exceeded")
            row = {"name": name, "inputs": {str(p): digest(p) for p in inputs}, "differences": []}
            spef = Path(case["spef"]).resolve() if "spef" in case else None
            if spef is not None:
                if spef.stat().st_size > 64 * 1024 * 1024:
                    raise ValueError("SPEF input limit exceeded")
                row["inputs"][str(spef)] = digest(spef)
            evidence["cases"].append(row)
            run([str(args.rusteda), "init", "--workspace", str(workspace), "--format", "json"], directory, "init", json_output=True)
            command = [str(args.rusteda), "import", "--workspace", str(workspace), "--base-rev", "rev0", "--threads", "2", "--format", "json"]
            for path in libraries:
                command += ["--lib", str(path), "--corner", "tt"]
            command += ["--netlist", str(inputs[-2]), "--sdc", str(inputs[-1])]
            if spef is not None:
                command += ["--spef", str(spef), "--spef-corner", "tt"]
            run(command, directory, "import", json_output=True)
            native_run = run([str(args.ruststa), "run", "--workspace", str(workspace), "--base-rev", "rev1", "--threads", "2", "--max-state-gib", "1", "--max-paths", "128", "--format", "json"], directory, "run", json_output=True)
            row["scope"] = native_run["scope"]
            if row["scope"].get("cppr") != "off":
                raise RuntimeError("differential requires CPPR off in both tools")
            script = ['if {![info exists sta_crpr_enabled]} {error "required CPPR control unavailable"}',
                      "set sta_crpr_enabled 0"]
            script += ["read_liberty " + tcl_word(path) for path in libraries]
            script += ["read_verilog " + tcl_word(inputs[-2]), "link_design top", "read_sdc " + tcl_word(inputs[-1])]
            if spef is not None:
                script.append("read_spef " + tcl_word(spef))
            for i, endpoint in enumerate(endpoints):
                for transition in ("rise", "fall"):
                    script.append(f"report_checks -{transition}_to {tcl_word(endpoint)} -path_delay min_max -group_path_count 8 -endpoint_path_count 2 -fields {{slew input_pin}} -format json -digits 12 > {tcl_word(directory / f'ref-{i}-{transition}.json')}")
            script.append("exit")
            script_path = directory / "reference.tcl"
            script_path.write_text("\n".join(script) + "\n")
            run([str(args.reference), "-no_init", "-threads", "2", "-exit", str(script_path)], directory, "reference")
            reference = {}
            for i in range(len(endpoints)):
                for transition in ("rise", "fall"):
                    report = json.loads((directory / f"ref-{i}-{transition}.json").read_text())
                    if not report.get("checks"):
                        raise RuntimeError(f"reference did not analyze {endpoints[i]} {transition}")
                    for check in report["checks"]:
                        path = check["source_path"]
                        bound = "early" if check["path_type"] == "min" else "late"
                        for entry_index, path_entry in enumerate(path):
                            pin = path_entry["pin"]
                            values = reference.setdefault(pin, {})
                            for field in ("arrival", "slew"):
                                if field not in path_entry:
                                    continue
                                key = f"{field}_{bound}_ns"
                                if spef is not None and field == "arrival" and pin in case.get("driving_ports", []):
                                    row.setdefault("uncompared_reference_fields", {})[f"{pin}.{key}"] = "OpenSTA displays the pre-driver input delay at the port; all physical sink arrivals are compared"
                                    continue
                                value = path_entry[field] * 1e9
                                # OpenSTA prints input_delay before the virtual driving-cell
                                # increment at a top port, and includes that increment at its
                                # zero-R wire sink. RustSTA stores the arrived pad waveform.
                                # Compare the latter physical point, also independently checked
                                # as its named sink below; retain the representation adjustment.
                                if field == "arrival" and pin in case.get("driving_ports", []):
                                    sink = path[entry_index + 1] if entry_index + 1 < len(path) else {}
                                    if sink.get("net") != pin or "arrival" not in sink:
                                        raise RuntimeError("driving-port reference has no zero-R sink")
                                    value = sink["arrival"] * 1e9
                                    row.setdefault("reference_representation", {})[pin] = "port arrival includes the virtual driving-cell increment, observed at the zero-R wire sink"

                                merge = min if bound == "early" else max
                                values[key] = merge(values[key], value) if key in values else value
            if len(reference) > 4096:
                raise ValueError("reference path-row budget exceeded")
            row["compared_values"] = 0
            for i, (pin, expected) in enumerate(reference.items()):
                native = run([str(args.ruststa), "inspect", "--workspace", str(workspace), "--pin", pin, "--format", "json"], directory, f"pin-{i}", json_output=True)
                metrics = native["object"]
                for key, value in expected.items():
                    actual = metrics.get(key)
                    if actual is None:
                        raise RuntimeError(f"native missing {pin}.{key}")
                    error = abs(actual - value)
                    row["compared_values"] += 1
                    row["differences"].append({"pin": pin, "field": key, "native_ns": actual, "reference_ns": value, "absolute_ns": error})
            row["max_absolute_ns"] = max(d["absolute_ns"] for d in row["differences"])
            row["status"] = "passed" if row["max_absolute_ns"] <= args.tolerance_ns else "failed"
            output.write_text(json.dumps(evidence, indent=2))
        evidence["status"] = "passed" if all(c["status"] == "passed" for c in evidence["cases"]) else "failed"
    except Exception as error:
        evidence["status"] = "failed"
        evidence["error"] = str(error)
        raise
    finally:
        output.write_text(json.dumps(evidence, indent=2))
    print(output)
    return 0 if evidence["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
