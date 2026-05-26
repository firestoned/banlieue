# Why banlieue?

banlieue exists because **the VM control plane is fragmented**, and the cost of
that fragmentation lands on the people least able to absorb it: the application
team that just wants a VM.

This section is the long-form answer to *"why build this at all, when vSphere
has an API, Proxmox has an API, libvirt has an API, and so does everything else?"*

The argument is structured around four claims, each with its own page:

1. **[The problem](problem.md)** — every shop with more than one hypervisor
   builds the same glue twice. The user surface is the wrong place to put the
   variation.
2. **[The abstraction principle](abstraction-principle.md)** — banlieue believes
   in *least-touch* APIs: the smallest possible user-facing surface, with all the
   backend variation hidden behind a Kubernetes-native contract. Users describe
   *what* they want; providers decide *how*.
3. **[Least-touch workflow](least-touch.md)** — what "least touch on the user's
   workflow" means concretely. Swapping a provider, mixing providers in one
   cluster, and onboarding a new backend should not change the user's manifest.
4. **[CRD-only contract](crd-only-contract.md)** — the deliberate choice to have
   the main controller and the providers communicate **only** through the
   Kubernetes API. No gRPC, no REST, no shared library version-skew problem. The
   K8s API is the bus.

Two further pages frame banlieue against its neighbours and bound the scope:

- **[Comparisons](comparisons.md)** — where banlieue sits next to Kubevirt,
  Cluster API, Crossplane, Terraform, and direct hypervisor SDKs. None of them
  solve the same problem.
- **[Non-goals](non-goals.md)** — what banlieue deliberately refuses to be. This
  is just as important as what it *is*.

If you read just one page in this section, read
[The abstraction principle](abstraction-principle.md). It's the whole reason the
project exists.
