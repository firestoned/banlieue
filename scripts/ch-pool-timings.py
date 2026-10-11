#!/usr/bin/env python3
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
"""Stage-by-stage timings of a Cloud Hypervisor VirtualMachinePool, from pool
creation through claims, refills, release and pool deletion.

Produces the numbers in docs/src/reference/cloud-hypervisor-pool-timings.md.
Three subcommands, which `make ch-pool-timings` runs in order:

    run      create the pool and claims through the Kubernetes API, record
             every transition (events.jsonl), and log in to each guest once it
             is GuestReady to read its own boot timeline (guests/*.txt)
    journal  fetch each Cloud Hypervisor host's banlieue journal for the run
             window over SSH (host-journal.json)
    report   join the three sources into per-stage tables (report.md)

Every stage uses the most precise source available: object timestamps for
cluster stages (to the second), the host journal for host stages (to the
microsecond), the guest's journal for guest stages. The poll itself (about
3 s) only decides when to move on.

Settings (environment):

    BANLIEUE_POOLTIME_CLASS     VMClass, tpmEnabled for the vTPM path   (required)
    BANLIEUE_POOLTIME_IMAGE     VMImage, Deferred for an install        (required)
    BANLIEUE_POOLTIME_USER_DATA cloud-config file every member gets     (required)
    BANLIEUE_POOLTIME_OUT       output directory      (default target/ch-pool-timings/<UTC time>)
    BANLIEUE_POOLTIME_NAMESPACE                       (default banlieue-system)
    BANLIEUE_POOLTIME_WARM      warm members          (default 3)
    BANLIEUE_POOLTIME_PROVISIONING_TIMEOUT  seconds   (default 900)
    BANLIEUE_POOLTIME_ADMIN_CONTEXT   kube context that creates the pool (default: current)
    BANLIEUE_POOLTIME_CLAIM_MODES     space-separated: subject broker  (default subject)
        subject: claims created by BANLIEUE_POOLTIME_SUBJECT_CONTEXT naming
                 itself, through ADR-0047's subject-authorization policy
        broker:  claims created by the admin context on behalf of
                 BANLIEUE_POOLTIME_BROKER_SUBJECT; that identity must be in
                 the policy's `brokers` list (this script never edits it)
    BANLIEUE_POOLTIME_SUBJECT_CONTEXT kube context of the claiming user
    BANLIEUE_POOLTIME_SUBJECT_ID      its RAW subject (no API-server prefix)
    BANLIEUE_POOLTIME_BROKER_SUBJECT  subject a broker claims for (default agent-1)
    BANLIEUE_POOLTIME_ISSUER          claim issuer, must be allowlisted
                                      (default https://issuer.example.com)
    BANLIEUE_POOLTIME_SSH_HOSTS       Provider=ssh-host pairs, space-separated,
                                      when a Provider's name is not an SSH host
    BANLIEUE_POOLTIME_GUEST_USER      guest login for the timeline (default kairos)

The runner must reach each host over SSH (with passwordless sudo for
journalctl and systemctl) and each guest through its host (ssh -J).
"""
import datetime as dt
import glob
import json
import os
import re
import statistics
import subprocess
import sys
import threading
import time

POLL_S = 1
CLAIM_BIND_TIMEOUT_S = 300
WARM_TIMEOUT_S = 3600
RELEASE_TIMEOUT_S = 600
TTL_S = 3600
SSH_TIMEOUT_S = 8
GUEST_COLLECT_EVERY_S = 10
ANSI = re.compile(r"\x1b\[[0-9;]*m")


def env(name, default=None, required=False):
    v = os.environ.get(name, default)
    if required and not v:
        sys.exit(f"{name} is required (see scripts/ch-pool-timings.py)")
    return v


NS = env("BANLIEUE_POOLTIME_NAMESPACE", "banlieue-system")
POOL = "ch-pool-timings"


def kctl(context):
    return ["kubectl"] + (["--context", context] if context else []) + ["-n", NS]


ADMIN = kctl(env("BANLIEUE_POOLTIME_ADMIN_CONTEXT"))
SSH_OPTS = ["-o", f"ConnectTimeout={SSH_TIMEOUT_S}", "-o", "BatchMode=yes",
            "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
            "-o", "LogLevel=ERROR"]


def ssh_host(provider):
    pairs = dict(p.split("=", 1) for p in env("BANLIEUE_POOLTIME_SSH_HOSTS", "").split() if "=" in p)
    return pairs.get(provider, provider)


def now():
    return dt.datetime.now(dt.timezone.utc)


def ts(s):
    return dt.datetime.fromisoformat(s.replace("Z", "+00:00"))


# ---- run ----------------------------------------------------------------------

class Recorder:
    def __init__(self, out):
        self.f = open(os.path.join(out, "events.jsonl"), "a")
        self.seen = set()

    def log(self, phase, obj, event, **kw):
        rec = {"t": now().isoformat(), "phase": phase, "obj": obj, "event": event, **kw}
        self.f.write(json.dumps(rec) + "\n")
        self.f.flush()
        print(f"{rec['t'][11:23]} [{phase}] {obj}: {event} {kw if kw else ''}", flush=True)

    def once(self, key, phase, obj, event, **kw):
        if key not in self.seen:
            self.seen.add(key)
            self.log(phase, obj, event, **kw)


def kget(kind, label=None):
    cmd = ADMIN + ["get", kind, "-o", "json"] + (["-l", label] if label else [])
    r = subprocess.run(cmd, capture_output=True, text=True)
    return json.loads(r.stdout).get("items", []) if r.returncode == 0 else []


def kapply(base, obj, rec):
    r = subprocess.run(base + ["apply", "-f", "-"], input=json.dumps(obj), text=True, capture_output=True)
    if r.returncode != 0:
        rec.log("error", obj["metadata"]["name"], "apply_failed", stderr=r.stderr.strip())
        sys.exit(f"apply {obj['kind']}/{obj['metadata']['name']} failed: {r.stderr.strip()}")


def cond(o, kind):
    for c in (o.get("status") or {}).get("conditions") or []:
        if c["type"] == kind:
            return c
    return None


def snapshot(rec, phase):
    vms = kget("virtualmachines", f"banlieue.io/pool={POOL}")
    chms = {c["metadata"]["name"]: c for c in kget("cloudhypervisormachines.infrastructure.banlieue.io")}
    pools = [p for p in kget("virtualmachinepools") if p["metadata"]["name"] == POOL]
    claims = [c for c in kget("virtualmachineclaims") if c["spec"]["poolRef"]["name"] == POOL]
    for vm in vms:
        n = vm["metadata"]["name"]
        st = vm.get("status") or {}
        rec.once(("vm", n), phase, n, "vm_seen", created=vm["metadata"]["creationTimestamp"])
        c = cond(vm, "Scheduled")
        if c and c["status"] == "True":
            rec.once(("sched", n), phase, n, "scheduled", ltt=c["lastTransitionTime"],
                     provider=(st.get("scheduled") or {}).get("providerName"))
        r = cond(vm, "Ready")
        if r and r["status"] == "True":
            rec.once(("ready", n), phase, n, "vm_ready", ltt=r["lastTransitionTime"])
        if "banlieue.io/claim" in vm["metadata"].get("labels", {}):
            rec.once(("claimed", n), phase, n, "member_claimed")
        ch = chms.get(n)
        if not ch:
            continue
        cst = ch.get("status") or {}
        rec.once(("chm", n), phase, n, "infra_cr_seen", created=ch["metadata"]["creationTimestamp"])
        if cst.get("hostUid") is not None:
            rec.once(("uid", n), phase, n, "host_uid", host_uid=cst["hostUid"])
        g = cond(ch, "GuestReady")
        if g and g["status"] == "True":
            rec.once(("gr", n), phase, n, "guest_ready", ltt=g["lastTransitionTime"])
    for p in pools:
        st = p.get("status") or {}
        rec.once(("pool", phase, st.get("available"), st.get("provisioning"), st.get("claimed")),
                 phase, POOL, "pool_counts", available=st.get("available"),
                 provisioning=st.get("provisioning"), claimed=st.get("claimed"))
    for c in claims:
        st = c.get("status") or {}
        if st.get("phase"):
            rec.once(("claim", c["metadata"]["name"], st["phase"]), phase, c["metadata"]["name"],
                     f"claim_{st['phase']}", vm=(st.get("virtualMachineRef") or {}).get("name"),
                     boundAt=st.get("boundAt"), created=c["metadata"]["creationTimestamp"])
    return vms, chms, pools, claims


def wait(rec, phase, pred, timeout, what):
    t0 = time.time()
    while True:
        s = snapshot(rec, phase)
        if pred(*s):
            rec.log(phase, POOL, f"reached: {what}", seconds=round(time.time() - t0, 1))
            return s
        if time.time() - t0 > timeout:
            rec.log(phase, POOL, f"TIMEOUT: {what}", seconds=round(time.time() - t0, 1))
            sys.exit(f"timed out after {timeout}s waiting for: {what}")
        time.sleep(POLL_S)


def available(pools):
    return ((pools[0].get("status") or {}).get("available") or 0) if pools else 0


def bound(claims, names):
    phases = {c["metadata"]["name"]: (c.get("status") or {}).get("phase") for c in claims}
    return all(phases.get(n) == "Bound" for n in names)


def collect_guests(out, stop):
    """Once per GuestReady member: the installed boot's start and the report,
    from the guest's own journal, before the run deletes it."""
    os.makedirs(os.path.join(out, "guests"), exist_ok=True)
    user = env("BANLIEUE_POOLTIME_GUEST_USER", "kairos")
    probe = ("echo boot_start $(sudo journalctl -b 0 -o short-iso-precise --no-pager | sed -n 2p | cut -d' ' -f1); "
             "echo report_ran $(sudo journalctl -b 0 -o short-iso-precise --no-pager "
             "| grep -m1 'banlieue installed report\\|systemd-notify\\|phase=installed' | cut -d' ' -f1)")
    while not stop.is_set():
        for ch in kget("cloudhypervisormachines.infrastructure.banlieue.io"):
            n = ch["metadata"]["name"]
            st = ch.get("status") or {}
            g = cond(ch, "GuestReady")
            path = os.path.join(out, "guests", f"{n}.txt")
            if not n.startswith(POOL) or not g or g["status"] != "True" or not st.get("addresses") or os.path.exists(path):
                continue
            host = ssh_host(ch["spec"]["providerRef"]["name"])
            r = subprocess.run(["ssh"] + SSH_OPTS + ["-J", host, f"{user}@{st['addresses'][0]['address']}", probe],
                               capture_output=True, text=True)
            if r.returncode == 0 and "boot_start 20" in r.stdout:
                open(path, "w").write(r.stdout)
        stop.wait(GUEST_COLLECT_EVERY_S)


def cmd_run(out):
    warm = int(env("BANLIEUE_POOLTIME_WARM", "3"))
    rec = Recorder(out)
    stop = threading.Event()
    collector = threading.Thread(target=collect_guests, args=(out, stop), daemon=True)
    collector.start()
    user_data = open(env("BANLIEUE_POOLTIME_USER_DATA", required=True)).read()
    kapply(ADMIN, {"apiVersion": "v1", "kind": "ConfigMap",
                   "metadata": {"name": POOL, "namespace": NS}, "data": {"user-data": user_data}}, rec)
    kapply(ADMIN, {"apiVersion": "banlieue.io/v1alpha1", "kind": "VirtualMachinePool",
                   "metadata": {"name": POOL, "namespace": NS},
                   "spec": {"warmReplicas": warm, "maxReplicas": warm * 3, "maxSurge": warm,
                            "readiness": "GuestReady",
                            "provisioningTimeoutSeconds": int(env("BANLIEUE_POOLTIME_PROVISIONING_TIMEOUT", "900")),
                            "template": {"spec": {
                                "classRef": {"name": env("BANLIEUE_POOLTIME_CLASS", required=True)},
                                "imageRef": {"name": env("BANLIEUE_POOLTIME_IMAGE", required=True)},
                                "userData": {"configMapRef": {"name": POOL}},
                                "desiredPowerState": "PoweredOn", "migrationPolicy": "never"}}}}, rec)
    rec.log("warmup", POOL, "pool_created")
    wait(rec, "warmup", lambda v, c, p, cl: available(p) >= warm, WARM_TIMEOUT_S, f"{warm} available")

    for mode in env("BANLIEUE_POOLTIME_CLAIM_MODES", "subject").split():
        if mode == "subject":
            base = kctl(env("BANLIEUE_POOLTIME_SUBJECT_CONTEXT", required=True))
            subject = env("BANLIEUE_POOLTIME_SUBJECT_ID", required=True)
        elif mode == "broker":
            base, subject = ADMIN, env("BANLIEUE_POOLTIME_BROKER_SUBJECT", "agent-1")
        else:
            sys.exit(f"unknown claim mode {mode}")
        names = [f"{POOL}-{mode}-{i}" for i in range(1, 5)]

        def claim(n):
            kapply(base, {"apiVersion": "banlieue.io/v1alpha1", "kind": "VirtualMachineClaim",
                          "metadata": {"name": n, "namespace": NS},
                          "spec": {"poolRef": {"name": POOL}, "ttlSeconds": TTL_S,
                                   "subject": {"issuer": env("BANLIEUE_POOLTIME_ISSUER", "https://issuer.example.com"),
                                               "id": subject}}}, rec)
            rec.log(f"{mode}-claim", n, "claim_created")

        claim(names[0])
        wait(rec, f"{mode}-claim1", lambda v, c, p, cl: bound(cl, names[:1]), CLAIM_BIND_TIMEOUT_S, "one claim Bound")
        wait(rec, f"{mode}-refill1", lambda v, c, p, cl: available(p) >= warm, WARM_TIMEOUT_S, "Warm after one claim")
        for n in names[1:]:
            claim(n)
        wait(rec, f"{mode}-burst", lambda v, c, p, cl: bound(cl, names[1:]), CLAIM_BIND_TIMEOUT_S, "3 claims Bound")
        wait(rec, f"{mode}-refill3", lambda v, c, p, cl: available(p) >= warm, WARM_TIMEOUT_S, "Warm after 3 claims")

        v, c, p, cl = snapshot(rec, f"{mode}-release")
        members = {x["metadata"]["name"]: ((x.get("status") or {}).get("virtualMachineRef") or {}).get("name")
                   for x in cl if x["metadata"]["name"] in names}
        hosts = {n: (ssh_host(((x.get("status") or {}).get("scheduled") or {}).get("providerName") or ""),
                     ((c.get(n) or {}).get("status") or {}).get("hostUid"))
                 for x in v for n in [x["metadata"]["name"]]}
        for n in names:
            subprocess.run(base + ["delete", "virtualmachineclaim", n, "--wait=false"], capture_output=True)
            rec.log(f"{mode}-release", n, "claim_delete_requested", vm=members.get(n))
        t0, gone = time.time(), set()
        while time.time() - t0 < RELEASE_TIMEOUT_S:
            v, c, p, cl = snapshot(rec, f"{mode}-release")
            live_claims = {x["metadata"]["name"] for x in cl}
            live_vms = {x["metadata"]["name"] for x in v}
            for n, vm in members.items():
                if n not in live_claims:
                    rec.once(("claim_gone", n), f"{mode}-release", n, "claim_gone")
                if vm and vm not in live_vms:
                    rec.once(("vm_gone", vm), f"{mode}-release", vm, "vm_gone")
                host, uid = hosts.get(vm, (None, None))
                if host and uid is not None and vm not in gone:
                    r = subprocess.run(["ssh"] + SSH_OPTS + [host, f"systemctl show -p ActiveState --value banlieue-ch@{uid}"],
                                       capture_output=True, text=True)
                    if r.stdout.strip() in ("inactive", "failed", ""):
                        gone.add(vm)
                        rec.log(f"{mode}-release", vm, "host_unit_gone", host_uid=uid)
            if not live_claims & set(names) and len(gone) == len([m for m in members.values() if m]):
                rec.log(f"{mode}-release", POOL, "reached: claims, VMs and host units gone",
                        seconds=round(time.time() - t0, 1))
                break
            time.sleep(POLL_S)

    subprocess.run(ADMIN + ["delete", "virtualmachinepool", POOL, "--wait=false"], capture_output=True)
    rec.log("pool-delete", POOL, "pool_delete_requested")
    wait(rec, "pool-delete", lambda v, c, p, cl: not v and not p, RELEASE_TIMEOUT_S, "pool and members gone")
    subprocess.run(ADMIN + ["delete", "configmap", POOL], capture_output=True)
    stop.set()
    collector.join()
    rec.log("done", POOL, "run complete")


# ---- journal ------------------------------------------------------------------

def cmd_journal(out):
    events = [json.loads(line) for line in open(os.path.join(out, "events.jsonl"))]
    start = ts(events[0]["t"]) - dt.timedelta(seconds=SSH_TIMEOUT_S)
    end = ts(events[-1]["t"]) + dt.timedelta(minutes=1)
    hosts = {ssh_host(e["provider"]) for e in events if e.get("provider")}
    with open(os.path.join(out, "host-journal.json"), "w") as f:
        for host in sorted(hosts):
            r = subprocess.run(["ssh"] + SSH_OPTS + [host,
                                f"sudo journalctl -o json --no-pager --since '{start:%Y-%m-%d %H:%M:%S} UTC' "
                                f"--until '{end:%Y-%m-%d %H:%M:%S} UTC'"], capture_output=True, text=True)
            for line in r.stdout.splitlines():
                try:
                    j = json.loads(line)
                except ValueError:
                    continue
                if "banlieue" in (j.get("UNIT") or "") + (j.get("_SYSTEMD_UNIT") or ""):
                    j["_POOLTIME_HOST"] = host
                    f.write(json.dumps(j) + "\n")
            print(f"journal: {host}", flush=True)


# ---- report -------------------------------------------------------------------

# (label, from, to)
STAGES = [
    ("Scheduled; infrastructure CR created", "vm_created", "scheduled"),
    ("Guest uid allocated on the host", "vm_created", "uid_allocated"),
    ("vTPM manufactured (`swtpm_setup` run time)", "tpm_setup_started", "tpm_setup_finished"),
    ("swtpm started", "vm_created", "swtpm_start"),
    ("VMM unit started", "vm_created", "vmm_start"),
    ("VM created in the VMM; the installer boots", "vm_created", "vm_create"),
    ("`Ready` (the VM is running)", "vm_created", "vm_ready"),
    ("Install: installer boot to the installed system's kernel", "vm_create", "installed_boot_start"),
    ("Installed system's boot to its report", "installed_boot_start", "installed_report_ran"),
    ("Report to installer ejected", "installed_report_ran", "installer_ejected"),
    ("`GuestReady`, the member is claimable", "vm_created", "guest_ready"),
]
HOST_MARKS = [
    ("manufacturing the vTPM", "tpm_manufacture_requested"),
    ("starting swtpm", "swtpm_start"),
    ("starting VMM unit", "vmm_start"),
    ("creating VM", "vm_create"),
    ("ejecting the installer", "installer_ejected"),
    ("tearing down on the host", "host_teardown"),
]


def stage_marks(out):
    marks = {}

    def mark(member, key, t):
        if member:
            marks.setdefault(member, {}).setdefault(key, t)

    for line in open(os.path.join(out, "events.jsonl")):
        e = json.loads(line)
        n, k = e["obj"], e["event"]
        if not n.startswith(POOL + "-") or "-subject-" in n or "-broker-" in n:
            continue
        if k == "vm_seen":
            mark(n, "vm_created", ts(e["created"]))
        elif k == "scheduled":
            mark(n, "scheduled", ts(e["ltt"]))
        elif k == "vm_ready":
            mark(n, "vm_ready", ts(e["ltt"]))
        elif k == "guest_ready":
            mark(n, "guest_ready", ts(e["ltt"]))

    path = os.path.join(out, "host-journal.json")
    lines = [json.loads(line) for line in open(path)] if os.path.exists(path) else []
    lines.sort(key=lambda j: int(j["__REALTIME_TIMESTAMP"]))
    owner = {}
    for j in lines:
        msg = j.get("MESSAGE")
        msg = ANSI.sub("", bytes(msg).decode(errors="replace") if isinstance(msg, list) else (msg or ""))
        t = dt.datetime.fromtimestamp(int(j["__REALTIME_TIMESTAMP"]) / 1e6, dt.timezone.utc)
        unit = j.get("UNIT") or ""
        m = re.search(r"machine=(\S+)", msg)
        u = re.search(r"@(\d+)\.service", msg) or re.search(r"@(\d+)\.service", unit)
        key = (j["_POOLTIME_HOST"], u.group(1)) if u else None
        if "guest uid allocated" in msg and m:
            owner[(j["_POOLTIME_HOST"], re.search(r"host_uid=(\d+)", msg).group(1))] = m.group(1)
            mark(m.group(1), "uid_allocated", t)
            continue
        who = m.group(1) if m else owner.get(key)
        if "banlieue-swtpm-setup@" in unit and msg.startswith("Starting "):
            mark(who, "tpm_setup_started", t)
        elif "banlieue-swtpm-setup@" in unit and msg.startswith("Finished "):
            mark(who, "tpm_setup_finished", t)
        for text, k in HOST_MARKS:
            if text in msg:
                mark(who, k, t)

    for path in glob.glob(os.path.join(out, "guests", "*.txt")):
        n = os.path.basename(path)[:-len(".txt")]
        for line in open(path):
            k, _, v = line.strip().partition(" ")
            if k in ("boot_start", "report_ran") and v:
                mark(n, "installed_" + k, ts(v))
    return marks


def secs(a, b):
    return (b - a).total_seconds() if a and b else None


def summary(values):
    v = sorted(x for x in values if x is not None)
    if not v:
        return "n/a"
    return f"{statistics.median(v):.1f} ({v[0]:.1f}–{v[-1]:.1f})" if len(v) > 1 else f"{v[0]:.1f}"


def cmd_report(out):
    marks = stage_marks(out)
    healthy = [m for m, s in marks.items() if "guest_ready" in s]
    lost = [m for m, s in marks.items() if "guest_ready" not in s and "host_teardown" in s]
    lines = [f"# Cloud Hypervisor pool timings: {os.path.basename(out)}", "",
             f"{len(marks)} members; {len(healthy)} reached GuestReady; {len(lost)} reaped before reporting.", "",
             "| Stage | Median (min–max), s | n |", "| --- | --- | --- |"]
    for label, a, b in STAGES:
        vals = [secs(marks[m].get(a), marks[m].get(b)) for m in healthy]
        lines.append(f"| {label} | {summary(vals)} | {len([x for x in vals if x is not None])} |")
    lines += ["", "## Phases", ""]
    events = [json.loads(line) for line in open(os.path.join(out, "events.jsonl"))]
    lines += [f"- {e['phase']}: {e['event']} in {e.get('seconds')} s" for e in events
              if e["event"].startswith(("reached", "TIMEOUT"))]
    lines += ["", "## Claims", ""]
    claims = {}
    for e in events:
        if "-subject-" in e["obj"] or "-broker-" in e["obj"]:
            claims.setdefault(e["obj"], {})[e["event"]] = e
    for n, c in sorted(claims.items()):
        b = c.get("claim_Bound")
        parts = []
        if b and b.get("boundAt") and b.get("created"):
            parts.append(f"creationTimestamp→boundAt {secs(ts(b['created']), ts(b['boundAt'])):.0f} s")
        if "claim_delete_requested" in c and "claim_gone" in c:
            parts.append(f"delete→gone {secs(ts(c['claim_delete_requested']['t']), ts(c['claim_gone']['t'])):.1f} s")
        lines.append(f"- {n}: " + "; ".join(parts))
    if lost:
        lines += ["", "## Members reaped before reporting", ""] + [f"- {m}" for m in sorted(lost)]
    report = "\n".join(lines) + "\n"
    open(os.path.join(out, "report.md"), "w").write(report)
    print(report)


def main():
    if len(sys.argv) < 2 or sys.argv[1] not in ("run", "journal", "report"):
        sys.exit(__doc__)
    out = env("BANLIEUE_POOLTIME_OUT") or os.path.join("target", "ch-pool-timings", now().strftime("%Y%m%dT%H%M%SZ"))
    os.makedirs(out, exist_ok=True)
    {"run": cmd_run, "journal": cmd_journal, "report": cmd_report}[sys.argv[1]](out)


if __name__ == "__main__":
    main()
