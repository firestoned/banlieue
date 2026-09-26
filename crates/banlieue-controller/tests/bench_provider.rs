// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Provider benchmark: the same `VirtualMachine`, measured the same way, on
//! any backend (docs/src/reference/provider-comparison.md).
//!
//! It uses only banlieue's API and SSH, so one run against libvirt, vSphere,
//! Proxmox or Cloud Hypervisor is directly comparable with another. Each run
//! creates a `VirtualMachine` with an SSH key in its user-data, records when
//! each stage is reached, logs in, runs one fixed workload as root, deletes
//! the VM and records how long deletion took. One JSON line per run is
//! appended to the output file; nothing identifying the host (names,
//! addresses) is recorded, only the CPU model the guest sees.
//!
//! `#[ignore]`d: running it is a request to create VMs; a missing setting
//! fails loudly.
//!
//! ```sh
//! export KUBECONFIG=~/.kube/<cluster>.yaml
//! BANLIEUE_BENCH_PROVIDER=<provider>     # failure domain label `name=` (the Provider's name)
//! BANLIEUE_BENCH_IMAGE=<vmimage>         # ready for that Provider
//! BANLIEUE_BENCH_CLASS=<vmclass>         # 2 vCPU / 4 GiB for the published table
//! BANLIEUE_BENCH_LABEL=libvirt           # the column this run belongs to
//!   cargo test -p banlieue-controller --test bench_provider -- --ignored --nocapture
//! ```
//!
//! Optional: `BANLIEUE_BENCH_NAMESPACE` (default: the Provider's),
//! `BANLIEUE_BENCH_RUNS` (default 3), `BANLIEUE_BENCH_OUT` (default
//! `target/provider-bench/<label>.jsonl`), `BANLIEUE_BENCH_SSH_USER`
//! (default `bench`, created by the user-data), `BANLIEUE_BENCH_PLACEMENT=any`
//! (no selector, for a provider too old to label its failure domain, on a
//! cluster where it is the only one), `BANLIEUE_BENCH_KEEP` (leave
//! the VM running after one run, for debugging). The runner must reach the
//! guests' addresses on port 22. Or `make provider-bench`; render the table
//! with `scripts/provider-bench-table.py`.

use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use banlieue_api::banlieue::{Provider, VirtualMachine};
use k8s_openapi::api::core::v1::Secret;
use kube::api::{Api, DeleteParams, PostParams};
use kube::{Client, ResourceExt};
use serde_json::{Map, Value, json};

const DEFAULT_RUNS: usize = 3;
const DEFAULT_USER: &str = "bench";
/// Fine enough to separate stages a second apart.
const POLL: Duration = Duration::from_millis(500);
/// Boot to an address. A Deferred first boot installs, so allow for it.
const READY_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Scheduling is a pure function of what the controller sees; if it has not
/// happened by now, it will not (no controller running, or no Provider
/// matches), and the run says why rather than waiting out the boot budget.
const SCHEDULE_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const SSH_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const DELETE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// A guest is settled once its boot has stayed the same, and systemd has
/// finished starting, for this long. Some images reboot once after their
/// first boot (Kairos grows its persistent partition); a workload run
/// before that measures a system that is about to disappear.
const SETTLE_QUIET: Duration = Duration::from_secs(60);
const SETTLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SETTLE_POLL: Duration = Duration::from_secs(5);
const SSH_PORT: u16 = 22;
/// The workload itself can take a few minutes on a slow backend.
const WORKLOAD_TIMEOUT_SECS: &str = "900";

/// One fixed workload, run as root in the guest, printing `BENCH key value`.
/// The same script as the VMM-level benchmark in the comparison doc, so the
/// two tables are comparable too.
const WORKLOAD: &str = r#"
p() { echo "BENCH $*"; }
thr() { awk -F, '/copied/{print $NF}' | tr -d ' '; }
t() { date +%s.%N; }
sync; echo 3 > /proc/sys/vm/drop_caches
s=$(t); dd if=/dev/zero bs=1M count=4096 2>/dev/null | sha256sum >/dev/null; e=$(t)
p cpu_sha256_4GiB_s $(awk -v a=$s -v b=$e 'BEGIN{printf "%.2f", b-a}')
s=$(t); i=0; while [ $i -lt 300000 ]; do i=$((i+1)); done; e=$(t)
p cpu_shell_loop_300k_s $(awk -v a=$s -v b=$e 'BEGIN{printf "%.2f", b-a}')
p mem_copy_32GiB $(dd if=/dev/zero of=/dev/null bs=1M count=32768 2>&1 | thr)
d=/var/tmp; [ -d /usr/local ] && [ -w /usr/local ] && d=/usr/local
p disk_seq_write_direct_1GiB $(dd if=/dev/zero of=$d/bench.bin bs=1M count=1024 oflag=direct 2>&1 | thr)
p disk_seq_read_direct_1GiB $(dd if=$d/bench.bin of=/dev/null bs=1M iflag=direct 2>&1 | thr)
p disk_4k_write_direct_64MiB $(dd if=/dev/zero of=$d/bench4k.bin bs=4k count=16384 oflag=direct 2>&1 | thr)
p disk_4k_read_direct_64MiB $(dd if=$d/bench4k.bin of=/dev/null bs=4k iflag=direct 2>&1 | thr)
rm -f $d/bench.bin $d/bench4k.bin
p guest_cpu_model "$(awk -F': ' '/model name/{print $2; exit}' /proc/cpuinfo)"
p guest_vcpus $(nproc)
p guest_mem_kib $(awk '/MemTotal/{print $2}' /proc/meminfo)
p guest_systemd_analyze "$(systemd-analyze time 2>/dev/null | head -1 | sed 's/Startup finished in //')"
p DONE
"#;

struct Settings {
    provider: String,
    image: String,
    class: String,
    label: String,
    namespace: Option<String>,
    runs: usize,
    out: PathBuf,
    user: String,
    /// Pin to the Provider's failure domain (`name=`), or place anywhere.
    pin: bool,
}

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("{name} is not set; see the module docs of tests/bench_provider.rs")
    })
}

fn settings() -> Settings {
    let label = required("BANLIEUE_BENCH_LABEL");
    Settings {
        provider: required("BANLIEUE_BENCH_PROVIDER"),
        image: required("BANLIEUE_BENCH_IMAGE"),
        class: required("BANLIEUE_BENCH_CLASS"),
        namespace: std::env::var("BANLIEUE_BENCH_NAMESPACE").ok(),
        runs: std::env::var("BANLIEUE_BENCH_RUNS")
            .ok()
            .and_then(|r| r.parse().ok())
            .unwrap_or(DEFAULT_RUNS),
        out: std::env::var("BANLIEUE_BENCH_OUT").map_or_else(
            |_| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/provider-bench")
                    .join(format!("{label}.jsonl"))
            },
            PathBuf::from,
        ),
        user: std::env::var("BANLIEUE_BENCH_SSH_USER").unwrap_or_else(|_| DEFAULT_USER.into()),
        pin: std::env::var("BANLIEUE_BENCH_PLACEMENT").as_deref() != Ok("any"),
        label,
    }
}

/// Seconds since `t0`, to the millisecond.
fn since(t0: Instant) -> f64 {
    (t0.elapsed().as_secs_f64() * 1000.0).round() / 1000.0
}

async fn wait_for<T, F, Fut>(what: &str, timeout: Duration, mut check: F) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let start = Instant::now();
    loop {
        if let Some(v) = check().await {
            return Ok(v);
        }
        if start.elapsed() > timeout {
            return Err(format!("timed out after {timeout:?} waiting for {what}"));
        }
        tokio::time::sleep(POLL).await;
    }
}

fn condition_true(vm: &VirtualMachine, kind: &str) -> bool {
    vm.status.as_ref().is_some_and(|s| {
        s.conditions
            .iter()
            .any(|c| c.type_ == kind && c.status == "True")
    })
}

/// A throwaway ed25519 key pair; returns (private key path, public key).
fn keypair(dir: &Path) -> (PathBuf, String) {
    let key = dir.join("id_ed25519");
    let ok = Command::new("ssh-keygen")
        .args([
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "banlieue-bench",
            "-f",
        ])
        .arg(&key)
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "ssh-keygen");
    let public = std::fs::read_to_string(key.with_extension("pub")).expect("public key");
    (key, public.trim().to_string())
}

/// cloud-init and Kairos (yip) both read `users:`; `admin` is Kairos's
/// sudo group, `sudo:` is cloud-init's. Each ignores the other's key.
fn user_data(user: &str, public_key: &str) -> String {
    format!(
        "#cloud-config\nusers:\n  - name: {user}\n    groups: [admin]\n    sudo: \"ALL=(ALL) NOPASSWD:ALL\"\n    shell: /bin/bash\n    ssh_authorized_keys:\n      - {public_key}\n"
    )
}

fn ssh(key: &Path, user: &str, address: &str) -> Command {
    let mut c = Command::new("ssh");
    c.args([
        "-i",
        &key.display().to_string(),
        "-o",
        "StrictHostKeyChecking=no",
        "-o",
        "UserKnownHostsFile=/dev/null",
        "-o",
        "LogLevel=ERROR",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=5",
        &format!("{user}@{address}"),
    ]);
    c
}

/// Wait until the guest's boot ID has not changed, and systemd has settled
/// (`running` or `degraded`), for [`SETTLE_QUIET`]. Returns when that quiet
/// window began and how many distinct boots were seen.
async fn settle(key: &Path, user: &str, address: &str) -> Result<(Instant, usize), String> {
    let start = Instant::now();
    let mut boots: Vec<String> = Vec::new();
    let mut stable_since: Option<Instant> = None;
    loop {
        let out = ssh(key, user, address)
            .args([
                "cat /proc/sys/kernel/random/boot_id; systemctl is-system-running 2>/dev/null || true",
            ])
            .stdin(Stdio::null())
            .output();
        let seen = out.ok().filter(|o| o.status.success()).map(|o| {
            let text = String::from_utf8_lossy(&o.stdout).to_string();
            let mut lines = text.lines();
            (
                lines.next().unwrap_or_default().trim().to_string(),
                lines.next().unwrap_or_default().trim().to_string(),
            )
        });
        match seen {
            Some((boot, state)) if !boot.is_empty() => {
                if boots.last() != Some(&boot) {
                    boots.push(boot);
                    stable_since = None;
                }
                let up = matches!(state.as_str(), "running" | "degraded");
                if !up {
                    stable_since = None;
                } else if stable_since.is_none() {
                    stable_since = Some(Instant::now());
                }
                if let Some(since) = stable_since
                    && since.elapsed() >= SETTLE_QUIET
                {
                    return Ok((since, boots.len()));
                }
            }
            // Unreachable: rebooting. The next answer is a new boot.
            _ => stable_since = None,
        }
        if start.elapsed() > SETTLE_TIMEOUT {
            return Err(format!(
                "the guest did not settle within {SETTLE_TIMEOUT:?} ({} boots seen)",
                boots.len()
            ));
        }
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// `BENCH key value` lines into a JSON object.
fn parse_workload(out: &str) -> Map<String, Value> {
    out.lines()
        .filter_map(|l| l.strip_prefix("BENCH "))
        .filter_map(|l| l.split_once(' ').or(Some((l, ""))))
        .filter(|(k, _)| *k != "DONE")
        .map(|(k, v)| (k.to_string(), Value::from(v.trim())))
        .collect()
}

async fn one_run(
    s: &Settings,
    run: usize,
    ns: &str,
    client: &Client,
    key: &Path,
    public_key: &str,
) -> Result<Value, String> {
    let name = format!("bench-{}-{}-{run}", s.label, std::process::id());
    // A Secret, not a ConfigMap: every API version accepts `secretRef`, and
    // it is where user-data belongs.
    let secrets: Api<Secret> = Api::namespaced(client.clone(), ns);
    let vms: Api<VirtualMachine> = Api::namespaced(client.clone(), ns);
    let secret: Secret = serde_json::from_value(json!({
        "metadata": {"name": name},
        "stringData": {"user-data": user_data(&s.user, public_key)},
    }))
    .map_err(|e| e.to_string())?;
    secrets
        .create(&PostParams::default(), &secret)
        .await
        .map_err(|e| format!("creating Secret: {e}"))?;
    // Every current provider labels its failure domains `name=<Provider>`;
    // `BANLIEUE_BENCH_PLACEMENT=any` is for an older one that does not, on
    // a cluster where it is the only Provider.
    let placement = if s.pin {
        json!({"failureDomainSelector": {"matchLabels": {"name": s.provider}}})
    } else {
        json!({})
    };
    let vm: VirtualMachine = serde_json::from_value(json!({
        "apiVersion": "banlieue.io/v1alpha1",
        "kind": "VirtualMachine",
        "metadata": {"name": name},
        "spec": {
            "classRef": {"name": s.class},
            "imageRef": {"name": s.image},
            "placement": placement,
            "userData": {"secretRef": {"name": name, "key": "user-data"}},
            "desiredPowerState": "PoweredOn",
            "migrationPolicy": "never"
        }
    }))
    .map_err(|e| e.to_string())?;

    let t0 = Instant::now();
    vms.create(&PostParams::default(), &vm)
        .await
        .map_err(|e| format!("creating VirtualMachine: {e}"))?;
    let mut timings = Map::new();
    let measured: Result<(), String> = async {
        let get = || async { vms.get_opt(&name).await.ok().flatten() };
        if let Err(e) = wait_for("scheduling", SCHEDULE_TIMEOUT, || async {
            get().await?.status?.scheduled.map(|_| ())
        })
        .await
        {
            let why = get()
                .await
                .and_then(|v| v.status)
                .map(|st| {
                    st.conditions
                        .iter()
                        .map(|c| format!("{}={} {}: {}", c.type_, c.status, c.reason, c.message))
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| "no status at all: is banlieue-controller running?".into());
            return Err(format!("{e} ({why})"));
        }
        timings.insert("scheduled_s".into(), since(t0).into());
        wait_for("provisioning", READY_TIMEOUT, || async {
            (get().await?.status?.initialization.provisioned == Some(true)).then_some(())
        })
        .await?;
        timings.insert("provisioned_s".into(), since(t0).into());
        wait_for("Ready", READY_TIMEOUT, || async {
            condition_true(&get().await?, "Ready").then_some(())
        })
        .await?;
        timings.insert("ready_s".into(), since(t0).into());
        let address = wait_for("an address", READY_TIMEOUT, || async {
            Some(get().await?.status?.addresses.first()?.address.clone())
        })
        .await?;
        timings.insert("address_s".into(), since(t0).into());
        let sock: SocketAddr = format!("{address}:{SSH_PORT}")
            .parse()
            .map_err(|e| format!("address {address:?}: {e}"))?;
        wait_for("sshd", SSH_TIMEOUT, || async move {
            TcpStream::connect_timeout(&sock, CONNECT_TIMEOUT)
                .ok()
                .map(drop)
        })
        .await?;
        timings.insert("ssh_port_s".into(), since(t0).into());
        let key_owned = key.to_path_buf();
        let user = s.user.clone();
        let addr = address.clone();
        wait_for("an SSH login", SSH_TIMEOUT, || {
            let (key, user, addr) = (key_owned.clone(), user.clone(), addr.clone());
            async move {
                ssh(&key, &user, &addr)
                    .arg("true")
                    .stdin(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
                    .then_some(())
            }
        })
        .await?;
        timings.insert("login_s".into(), since(t0).into());

        let (settled_at, boots) = settle(key, &s.user, &address).await?;
        timings.insert(
            "settled_s".into(),
            (settled_at - t0).as_secs_f64().round().into(),
        );
        timings.insert("boots".into(), boots.into());

        let started = Instant::now();
        let mut child = ssh(key, &s.user, &address)
            .args(["sudo", "timeout", WORKLOAD_TIMEOUT_SECS, "sh", "-s"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("ssh: {e}"))?;
        child
            .stdin
            .take()
            .ok_or("ssh stdin")?
            .write_all(WORKLOAD.as_bytes())
            .map_err(|e| e.to_string())?;
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        let bench = parse_workload(&String::from_utf8_lossy(&out.stdout));
        if bench.is_empty() {
            return Err("the workload produced nothing (does the user have sudo?)".into());
        }
        timings.insert("workload_s".into(), since(started).into());
        timings.insert("bench".into(), Value::Object(bench));
        Ok(())
    }
    .await;

    if std::env::var("BANLIEUE_BENCH_KEEP").is_ok() {
        let kept_key = s.out.with_extension("key");
        let _ = std::fs::copy(key, &kept_key);
        println!(
            "BANLIEUE_BENCH_KEEP: leaving VirtualMachine {ns}/{name} running; ssh -i {} {}@<address>",
            kept_key.display(),
            s.user
        );
        measured?;
        return Ok(json!({"label": s.label, "kept": name, "timings_s": timings}));
    }
    let deleted_at = Instant::now();
    let _ = vms.delete(&name, &DeleteParams::default()).await;
    let _ = secrets.delete(&name, &DeleteParams::default()).await;
    let gone = wait_for(
        "the VirtualMachine to be deleted",
        DELETE_TIMEOUT,
        || async { vms.get_opt(&name).await.ok()?.is_none().then_some(()) },
    )
    .await;
    measured?;
    gone?;
    timings.insert("delete_s".into(), since(deleted_at).into());
    let bench = timings.remove("bench").unwrap_or_default();
    Ok(json!({
        "label": s.label,
        "run": run,
        "class": s.class,
        "image": s.image,
        "date": chrono::Utc::now().format("%Y-%m-%d").to_string(),
        "timings_s": timings,
        "bench": bench,
    }))
}

#[tokio::test]
#[ignore = "creates VirtualMachines on a real cluster: see the module docs"]
async fn benchmark_a_provider() {
    let s = settings();
    let client = Client::try_default().await.expect("a reachable cluster");
    let provider = Api::<Provider>::all(client.clone())
        .list(&Default::default())
        .await
        .expect("listing Providers")
        .into_iter()
        .find(|p| p.name_any() == s.provider)
        .unwrap_or_else(|| panic!("no Provider {}", s.provider));
    let ns = s
        .namespace
        .clone()
        .or_else(|| provider.namespace())
        .expect("a namespace");
    let dir = tempfile::tempdir().expect("a temporary directory");
    let (key, public_key) = keypair(dir.path());
    if let Some(parent) = s.out.parent() {
        std::fs::create_dir_all(parent).expect("the output directory");
    }

    let mut failures = Vec::new();
    for run in 1..=s.runs {
        match one_run(&s, run, &ns, &client, &key, &public_key).await {
            Ok(mut line) => {
                line["provider_class"] = Value::from(provider.spec.provider_class_ref.name.clone());
                println!("{line}");
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&s.out)
                    .expect("the output file");
                writeln!(f, "{line}").expect("writing a result");
            }
            Err(e) => {
                println!("run {run} failed: {e}");
                failures.push(format!("run {run}: {e}"));
            }
        }
    }
    println!("results appended to {}", s.out.display());
    assert!(failures.is_empty(), "{failures:?}");
}
