#!/usr/bin/env python3
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
"""Render provider benchmark results as the markdown table in
docs/src/reference/provider-comparison.md.

Input: JSON lines written by crates/banlieue-controller/tests/bench_provider.rs
(`make provider-bench`), one file per provider or all in one. Output: one
row per metric, one column per provider label, each cell the median and the
min-max range over the runs.

    scripts/provider-bench-table.py target/provider-bench/*.jsonl
"""
import json
import re
import statistics
import sys

# The published columns, in this order; any other label follows.
COLUMN_ORDER = ["cloud-hypervisor", "libvirt", "vsphere", "proxmox"]
UNITS = {"GB/s": 1000.0, "MB/s": 1.0, "kB/s": 0.001}

# (key, section, label, unit, lower_is_better)
METRICS = [
    ("scheduled_s", "timings_s", "VirtualMachine created → scheduled", "s", True),
    ("provisioned_s", "timings_s", "→ infrastructure provisioned", "s", True),
    ("ready_s", "timings_s", "→ `Ready`", "s", True),
    ("address_s", "timings_s", "→ first address", "s", True),
    ("ssh_port_s", "timings_s", "→ sshd answers", "s", True),
    ("login_s", "timings_s", "→ SSH login", "s", True),
    ("settled_s", "timings_s", "→ settled (boot stable 60 s, systemd up)", "s", True),
    ("boots", "timings_s", "Boots before settled", "count", True),
    ("delete_s", "timings_s", "delete → VirtualMachine gone", "s", True),
    ("cpu_sha256_4GiB_s", "bench", "CPU: sha256 of 4 GiB", "s", True),
    ("cpu_shell_loop_300k_s", "bench", "CPU: shell loop, 300k", "s", True),
    ("mem_copy_32GiB", "bench", "Memory: copy 32 GiB", "MB/s", False),
    ("disk_seq_write_direct_1GiB", "bench", "Disk: sequential write, 1 GiB, direct", "MB/s", False),
    ("disk_seq_read_direct_1GiB", "bench", "Disk: sequential read, 1 GiB, direct", "MB/s", False),
    ("disk_4k_write_direct_64MiB", "bench", "Disk: 4 KiB writes, 64 MiB, direct", "MB/s", False),
    ("disk_4k_read_direct_64MiB", "bench", "Disk: 4 KiB reads, 64 MiB, direct", "MB/s", False),
]


def number(v):
    if isinstance(v, (int, float)):
        return float(v)
    m = re.match(r"([\d.]+)\s*(GB/s|MB/s|kB/s)?$", str(v).strip())
    if not m:
        return None
    return float(m.group(1)) * UNITS.get(m.group(2) or "", 1.0)


def cell(values, unit=""):
    if not values:
        return "—"
    med = statistics.median(values)
    whole = med >= 100 or unit == "count"
    fmt = (lambda x: f"{x:.0f}") if whole else (lambda x: f"{x:.1f}")
    if len(values) == 1:
        return fmt(med)
    return f"{fmt(med)} ({fmt(min(values))}–{fmt(max(values))})"


def main(paths):
    rows = []
    for path in paths:
        with open(path) as f:
            rows += [json.loads(line) for line in f if line.strip().startswith("{")]
    if not rows:
        sys.exit("no results")
    labels = sorted({r["label"] for r in rows},
                    key=lambda l: (COLUMN_ORDER.index(l) if l in COLUMN_ORDER else len(COLUMN_ORDER), l))
    runs = {l: [r for r in rows if r["label"] == l] for l in labels}

    print("| Metric | " + " | ".join(f"`{l}`" for l in labels) + " |")
    print("| --- | " + " | ".join("---" for _ in labels) + " |")
    for key, section, name, unit, lower in METRICS:
        better = "lower is better" if lower else "higher is better"
        cells = []
        for l in labels:
            vals = [number(r.get(section, {}).get(key)) for r in runs[l]]
            cells.append(cell([v for v in vals if v is not None], unit))
        print(f"| {name} ({unit}, {better}) | " + " | ".join(cells) + " |")
    print("| Runs | " + " | ".join(str(len(runs[l])) for l in labels) + " |")
    print()
    for l in labels:
        cpu = {r.get("bench", {}).get("guest_cpu_model") for r in runs[l]} - {None}
        dates = sorted({r.get("date") for r in runs[l]} - {None})
        cls = sorted({r.get("class") for r in runs[l]})
        print(f"- `{l}`: guest CPU {', '.join(sorted(cpu)) or 'unknown'}; "
              f"class {', '.join(cls)}; measured {', '.join(dates)}.")


if __name__ == "__main__":
    main(sys.argv[1:])
