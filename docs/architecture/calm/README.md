# banlieue CALM Architecture

This folder contains the [FINOS Common Architecture Language Model
(CALM)](https://calm.finos.org/) description of banlieue.

| File | Purpose |
| --- | --- |
| `architecture.json` | Single architecture document: nodes, relationships, flows, controls, metadata. Targets CALM schema **1.2**. |
| `templates/mermaid/system.md.hbs` | Handlebars template that renders every node and relationship as a single Mermaid `flowchart LR`. Output → `docs/src/architecture/system.md`. |
| `templates/mermaid/flows.md.hbs` | Handlebars template that renders each `flows[]` entry as its own Mermaid `flowchart TD`. Output → `docs/src/architecture/flows.md`. |

## What it models

- **Actors** — the VM consumer (the user banlieue is *for*) and the platform
  operator (the admin who installs CRDs / Providers).
- **Ecosystem** — the management Kubernetes cluster (everything banlieue
  ships runs here).
- **Services** — the K8s API server, the `banlieue-controller`, and the
  three planned provider controllers (`banlieue-provider-{vsphere,proxmox,
  libvirt}`). Provider node names include their roadmap phase so the
  diagram is honest about what's implemented vs planned.
- **Networks** — the real backends each provider targets (vSphere/vCenter,
  Proxmox VE, libvirt/KVM).
- **Data assets** — every banlieue CRD: `VirtualMachine`, `Provider`,
  `VMClass`, `VMImage`, and the infrastructure machine CR (`VSphereMachine`,
  etc.).
- **Relationships** — every wire is an HTTPS call to the K8s API server.
  *No direct controller-to-controller arrow exists*, by design — that is
  the CRD-only contract encoded as architecture.
- **Flows** — *Create*, *Swap*, *Delete*. The Swap flow is the canonical
  demonstration of the least-touch principle (`docs/src/reasoning/least-touch.md`).
- **Controls** — references to the project's non-negotiables: CRD-only
  contract, least-touch principle, code-first CRDs, the CAPI v1beta2
  InfraMachine contract, and the existing supply-chain scanning posture.
  Each control links to NIST SP 800-53 Rev. 5 / SP 800-218 (SSDF) and to
  in-repo evidence files.

## Validating

```bash
make calm-validate
```

(Under the hood: `npx --yes @finos/calm-cli@1.37.0 validate -a docs/architecture/calm/architecture.json -f pretty`.)

## Rendering the Mermaid diagrams

```bash
make calm-diagrams
```

Writes `docs/src/architecture/system.md` and `docs/src/architecture/flows.md`,
which the MkDocs build then picks up under the **Concepts → Architecture**
section.

`make docs` runs `calm-diagrams` automatically before building MkDocs.

## CI: reusable CALM workflow

A reusable GitHub Actions workflow lives at
[`.github/workflows/calm.yaml`](../../../.github/workflows/calm.yaml) and wraps
the CALM CLI. Call it from any other workflow with `workflow_call`:

```yaml
jobs:
  validate:
    uses: ./.github/workflows/calm.yaml
    with:
      command: validate
      architecture: docs/architecture/calm/architecture.json
      strict: true

  mermaid:
    uses: ./.github/workflows/calm.yaml
    with:
      command: template
      architecture: docs/architecture/calm/architecture.json
      template-dir: docs/architecture/calm/templates/mermaid
      output: docs/src/architecture
      clear-output-directory: true
      upload-artifact: true
      artifact-name: calm-mermaid
```

Pin a specific CLI version with `cli-version: "1.37.0"` (that is the default).

## Updating

When you add a new provider, controller subsystem, or external backend:

1. Add a node with a stable `unique-id`.
2. Wire it into the appropriate `deployed-in` / `composed-of` relationship
   (likely `rel-mgmt-cluster-contains-controllers` or
   `rel-kube-api-stores-crs`).
3. Add a `connects` relationship to the K8s API (every provider has one) and
   a second `connects` relationship to its backend network/service.
4. If a flow now traverses the new edge, add a transition.
5. Run `make calm-validate` and then `make calm-diagrams` to refresh the
   rendered docs.
