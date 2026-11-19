# 25: AWS Lambda MicroVMs provider, a managed sandbox backend

> **Goal.** Decide whether AWS Lambda MicroVMs belong behind banlieue's
> `VirtualMachine`, pool and claim API as a managed sandbox backend, and if
> they do, build a `lambda-microvms` provider class whose members are
> Lambda MicroVMs launched from a `VMImage` banlieue published. Lambda
> MicroVMs are Firecracker microVMs that AWS runs: the same kind of
> sandbox roadmap 20 builds on hosts the operator owns, with nobody to
> patch and nothing to rack.
>
> **Stop condition (if phase 0 says go).** A `VMImage` of the MicroVM kind
> is published as a Lambda MicroVM image through roadmap 22's publish Job;
> a `VirtualMachine` on a `lambda-microvms` `Provider` becomes a running
> MicroVM whose run hook received its payload, reports its endpoint and
> `GuestReady`, suspends and resumes on `desiredPowerState`, and is
> terminated on delete or at its maximum lifetime, with the
> `VirtualMachine` saying which. A `VirtualMachinePool` of them warms,
> is claimed and refills, and its numbers sit beside Firecracker's and
> Cloud Hypervisor's in the provider comparison.
>
> **Stop condition (if phase 0 says no).** ADR-0168 records why, and this
> roadmap closes as decided rather than done.
>
> **Status (2026-11-19): not started.** Nothing in the tree yet. Depends on
> roadmap 22 phases 0 to C.

Baseline when written: `dca5f45` (2026-11-19). This roadmap takes **ADR-0168
and ADR-0169** from the block roadmaps 22 to 25 reserve (0158 to 0169).

## What the service is, as far as it is known

Lambda MicroVMs launched on 2026-06-22. Everything below comes from
third-party write-ups (one of them hands-on); the AWS announcement and API
reference could not be read when this roadmap was written. **Phase 0's
first task is to read the API reference and correct this section.**

- **Image.** Built by the service from a zip in S3 holding a Dockerfile
  (on a Lambda-managed base image, Amazon Linux 2023 minimal) and the
  content. The service runs the Dockerfile, starts the application, waits
  for an optional ready hook, and **takes a memory and disk snapshot**.
  Images are versioned; managed base images are deprecated, then expire.
- **MicroVM.** Launched from an image version by restoring that snapshot.
  One per tenant, session or job. Each has its own HTTPS endpoint; every
  request carries a short-lived token (`CreateMicrovmAuthToken`) in a
  header, and there is no unauthenticated mode. A shell token exists for an
  interactive terminal.
- **Lifecycle.** States reported as pending, running, suspending,
  suspended, terminating, terminated. Suspend and resume keep memory and
  disk (measured: about 2 s to resume). An idle policy can suspend and
  auto-resume on traffic. A hard **maximum lifetime of 8 hours**, suspended
  time included.
- **Per-launch input.** A run hook receives a JSON payload passed at launch
  (`runHookPayload`; the documented limit is 4 KiB or 16 KiB, sources
  disagree). No user-data in the cloud-init sense.
- **Sizes.** 0.5 to 8 GiB and 0.25 to 4 vCPU baseline, bursting to 4x; disk
  up to 32 GiB. Arm64 at launch; x86 support reported added in November
  2026.
- **No VPC subnet and no private IP reported**; no vTPM; no failure domains
  exposed; launch rate quota of a few per second per account.

## Why this is a fit question, not just another provider

Each row is a place where Lambda MicroVMs differ from every backend banlieue
has, and where ADR-0168 must decide before any code.

| Difference | What it collides with |
| --- | --- |
| **Every MicroVM restores the same snapshot.** | ADR-0052 rules out instant clones for vSphere, and roadmap 20 phase J gates Firecracker snapshot-restore behind ADR-0122, because a restored guest shares its parent's RNG state, identifiers and any secret in memory at snapshot time. Here the snapshot is the only way to launch. The ADR must scope ADR-0052 for this class: the image contract requires per-instance identity and entropy to be created in the run hook (ADR-0108: guest software is the consumer's image content), and banlieue states plainly that it cannot verify this. |
| **No vTPM.** | `tpmEnabled`, sealing (ADR-0088), EK publication (ADR-0045) and roadmap 18's isolation property do not apply. Refused at admission, as for Firecracker (roadmap 20). |
| **An endpoint, not an address.** | `VirtualMachine.status` and the InfraMachine contract's `addresses` carry IPs and DNS names; the endpoint is a DNS name, so it fits as `ExternalDNS`, but reaching it needs a token banlieue does not mint (below). |
| **Access needs a minted token.** | ADR-0097: banlieue's API ends at `VirtualMachineClaim`. Minting a per-request token for a claimant is a broker's job (mediatore), not banlieue's. banlieue publishes the MicroVM's identity on the claim; whoever brokers access calls `CreateMicrovmAuthToken`. |
| **8-hour maximum lifetime.** | A pool member that waits warm for hours expires. `VirtualMachinePool` must treat expiry as a member that was reaped, and the pool's warm strategy must budget for it; a claim on a member near expiry is a choice ADR-0135's strategies should see. |
| **Suspended is a real, cheap state.** | `PowerState::Suspended` exists but no backend uses it heavily. A pool whose warm members sit suspended (storage billed, compute not) and resume on claim may be the right model here; resume-on-traffic means a claimed member resumes on first use. |
| **No zones, no subnet.** | Placement is region only; spread across failure domains (ADR-0098) is meaningless on one `Provider`, and network classes do not apply. Egress control (ADR-0109) is whatever the service offers, which phase 0 must find out. |
| **The image is a Dockerfile, not a disk.** | `banlieue-imagebuilder` builds Kairos disks. A MicroVM `VMImage` is a build context; see "VMImage integration" below. |

The honest alternative ADR-0168 must weigh: this is an agent sandbox
service, and agent sandboxes moved to mediatore (ADR-0097, mediatore
ADR-0008). mediatore could call Lambda MicroVMs directly and skip banlieue.
The case for banlieue is one pool and claim model across on-premises
Firecracker, Cloud Hypervisor and a managed cloud backend, so a consumer
can burst from one to the other without changing how it claims.

## VMImage integration

Publishing is roadmap 22's ADR-0162; this roadmap adds the MicroVM kind:

- [ ] A `VMImage` source kind for a MicroVM build context (a Dockerfile and
      content, by OCI digest or a URL with a digest), declared explicitly,
      with the managed base image named by ARN and pinned (a base that
      expires is surfaced as a condition before it breaks launches).
- [ ] The publish Job zips the context, uploads it to an admin-named S3
      bucket (tagged, with a lifecycle rule), calls the image create API
      with a dedicated build role, waits for the build, and records the
      image ARN and version with the source digest on the `VMImage`'s
      per-Provider status.
- [ ] A rebuilt context is a new image version, never an update in place;
      versions banlieue created are deleted when no member uses them.
- [ ] A hand-built MicroVM image by ARN works as `Template`.
- [ ] Whether a Kairos-built root filesystem could be the Dockerfile's
      content (one image definition for roadmap 20 and this roadmap) is a
      spike question, expected to be no: the service owns the base.

## Phase 0 (gates everything)

- [ ] Read the AWS API reference and developer guide; correct "What the
      service is" above; record exact operation names, limits, regions,
      architectures and whether the service is GA.
- [ ] Rust access: is there an SDK crate for the service, or does it go
      through roadmap 22's first-party SigV4 client?
- [ ] Networking: is there any VPC attachment, egress control or private
      endpoint? What can a MicroVM reach by default?
- [ ] Measure: image build time, launch to run-hook 200, suspend and
      resume, at the sizes a sandbox pool would use.
- [ ] Prove the snapshot problem concretely: two MicroVMs from one image
      version, compare boot ID, machine ID, a key generated at build time
      and one generated in the run hook.
- [ ] Write ADR-0168 (go or no-go, with the fit table above answered). If
      no-go, close this roadmap.

## Phases (only if ADR-0168 says go)

| Phase | What | Status |
| --- | --- | --- |
| 0 | Above; ADR-0168 | ⛔ |
| A | ADR-0169: the InfraMachine (kind name, `providerID` from the MicroVM ARN, endpoint as `ExternalDNS`, `GuestReady` from the service's running state after the run hook succeeds, `Suspended` mapped both ways, expiry as a terminal reason), with what the class refuses at admission (`tpmEnabled`, `trustedBoot`, `firstBootSealing`, disk images, network classes, a payload over the limit) | ⛔ |
| B | Client behind a trait, with a fake that **rejects** what the service rejects (resume of a terminated MicroVM, launch from an expired base), per `rules/testing.md` rule 1; live protocol tier (`#[ignore]`, fails loudly without an account) | ⛔ |
| C | API, CRDs, `lambda-microvms` provider class, `spec.userData` carried as the run-hook payload, `regen-crds`, examples | ⛔ |
| D | Provider: launch, observe, suspend, resume, terminate, adopt by tag, orphan sweep (roadmap 22), launch-rate quota respected | ⛔ |
| E | VMImage integration above | ⛔ |
| F | Pools: expiry treated as reaping, suspended warm members if ADR-0168 chose them, a claim's member identity published for the broker; e2e warm, claim, release, refill | ⛔ |
| G | Docs and threat model: guide, provider comparison column, threat-model full pass for ADR-0168 and ADR-0169 (shared snapshot state recorded as an accepted risk with its *Revisit when*, not as a control banlieue has) | ⛔ |

## Out of scope

Minting access tokens for claimants (mediatore's broker); public access to
endpoints; any use of Lambda functions; running banlieue's own Firecracker
provider on AWS (roadmap 20 on a `.metal` instance is a separate idea).

## Read first

Roadmaps 20 and 22, ADR-0052, 0097, 0108, 0109, 0135 and 0137, and roadmap
20 phase J's questions for ADR-0122 (they are this roadmap's questions too).

## Definition of done

- [ ] ADR-0168 accepted, either way.
- [ ] If go: stop condition above verified live (no account ID, ARN or
      endpoint committed); ADR-0169 accepted with its threat-model pass;
      `cargo deny` clean; provider comparison and `ROADMAPS.md` updated.
