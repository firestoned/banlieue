<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# vSphere Least Privilege

The vSphere provider logs in to vCenter with the account in its
`Provider.spec.credentialsRef` Secret. That account is the most powerful
credential banlieue holds (see [threat model](threat-model.md) §1), so it
should carry a custom role with only the privileges below, assigned only on
the objects below. This page is derived from the vCenter calls the provider
makes today, enumerated from `crates/banlieue-provider-vsphere/src/client/vim.rs`
and `src/import.rs` (the only two files that talk to vCenter). It is not a
guess at what a VM provider usually needs.

!!! warning "Validate before you rely on it"
    Rows marked **unverified** below come from the code path, not from a
    test against vCenter with this exact role. Create the role, run
    `make vsphere-live-test` with it, and widen only what fails.

## What the provider calls

Transport is the VI/JSON API (`/sdk/vim25/{release}`) through `vim_rs`. The
provider never calls OvfManager, HttpNfcLease, Content Library, tagging,
custom attributes, CryptoManager, guest operations or `AcquireTicket`. It
therefore needs none of the privileges those require.

| Operation | Where | Target object | Why |
| --- | --- | --- | --- |
| `SessionManager.Login` / `Logout` | `vim.rs` client construction | SessionManager | Authenticate |
| `ViewManager.CreateContainerView`, property reads | `vim.rs` inventory listing | root folder, datacenters, clusters, datastores, networks, VMs, tasks | Inventory, reachability (ADR-0019), task polling |
| `Folder.CreateFolder` | `ensure_folder` | datacenter VM folder | Per-zone template and VM folders |
| `Folder.CreateVM_Task` | `import_iso_template` | VM folder, cluster root pool, datastore, port groups | Build an ISO install template (ADR-0021) |
| `VirtualMachine.ReconfigVM_Task` | template NIC slot, CD-ROM and boot order, CD-ROM removal, disk grow, vTPM add | VM | Template finishing, OS disk floor, vTPM (ADR-0039) |
| `VirtualMachine.MarkAsTemplate` | `import_iso_template` | VM | Finish a template |
| `VirtualMachine.PowerOnVM_Task` / `PowerOffVM_Task` / `SuspendVM_Task` | `set_power_state`, install boot | VM | Desired power state (ADR-0024) |
| `ClusterComputeResource.PlaceVm` | `clone_vm` | cluster | DRS host recommendation before clone |
| `VirtualMachine.CloneVM_Task` | `clone_vm` | template → folder, cluster root pool, datastore | Provision a `VSphereMachine` |
| `VirtualMachine.Destroy_Task` | `destroy_vm` | VM or template | Machine delete, image finalize, forced rebuild |
| `FileManager.MakeDirectory` | `ensure_datastore_dir` | datastore | ISO upload directory |
| HTTP `HEAD` / `PUT` / `DELETE` on `/folder/...` | `import.rs` | datastore | Check, upload and replace the install ISO |

The clone and create specs also carry: NIC backing edits, `extraConfig`
(`guestinfo.*`, `ethernetN.pciSlotNumber`), CPU and memory, a new disk and
SCSI/IDE controllers and a CD-ROM (template create only), firmware and
secure boot (create only), and a vTPM device. There is no storage policy
and no encryption spec; the only cryptographic operation is the vTPM add.

## The role

Create a custom role, for example `banlieue-sandbox`, with these privileges:

| Privilege | Needed for | Confidence |
| --- | --- | --- |
| `System.Anonymous`, `System.View`, `System.Read` | Login, container views, every property read. Included in every role | High |
| `Folder.Create` | `ensure_folder` | High |
| `VirtualMachine.Inventory.Create` | `CreateVM_Task` (template import) | High |
| `VirtualMachine.Inventory.CreateFromExisting` | `CloneVM_Task` | High |
| `VirtualMachine.Inventory.Delete` | `Destroy_Task` | High |
| `VirtualMachine.Provisioning.Clone` | `CloneVM_Task` | High |
| `VirtualMachine.Provisioning.DeployTemplate` | Clone whose source is a template | Medium |
| `VirtualMachine.Provisioning.MarkAsTemplate` | `MarkAsTemplate` | High |
| `VirtualMachine.Config.AddNewDisk` | Template create (new disk) | Medium |
| `VirtualMachine.Config.AddRemoveDevice` | Template devices, CD-ROM removal, vTPM add | High |
| `VirtualMachine.Config.EditDevice` | NIC backing on clone, CD-ROM edit | High |
| `VirtualMachine.Config.AdvancedConfig` | `extraConfig` (`guestinfo.*`, PCI slot) | High |
| `VirtualMachine.Config.CPUCount`, `VirtualMachine.Config.Memory` | Clone sizing from `VMClass` | High |
| `VirtualMachine.Config.DiskExtend` | OS disk floor (grow only) | High |
| `VirtualMachine.Config.Settings` | Boot order on the install template | High |
| `VirtualMachine.Interact.PowerOn`, `.PowerOff`, `.Suspend` | Power state | High |
| `VirtualMachine.Interact.SetCDMedia`, `.DeviceConnection` | Connecting the install ISO | **Unverified** |
| `Resource.AssignVMToPool` | Create and clone into the cluster's root pool | High |
| `Datastore.AllocateSpace` | Create, clone, disk grow, ISO upload | High |
| `Datastore.Browse` | ISO existence check, ISO-backed CD-ROM | Medium |
| `Datastore.FileManagement` | ISO directory, upload, replace | High |
| `Network.Assign` | NIC backings on create and clone | High |
| `Cryptographer.*` subset | Adding a vTPM (only if any `VMClass` sets `tpmEnabled`) | **Unverified**: likely `Cryptographer.Access` plus the encrypt-new and reconfigure privileges; needs a key provider on vCenter |

Leave out everything else, including `VirtualMachine.Interact.ConsoleInteract`,
guest operations, snapshot management, `Host.*` and all `Global.*`
privileges. The provider uses none of them.

If image builds run under a different Provider or account than VM
provisioning, you can split the role. Only the import path needs
`VirtualMachine.Inventory.Create`, `.Provisioning.MarkAsTemplate`,
`.Config.AddNewDisk`, `.Config.Settings`, the `Interact.SetCDMedia`
pair and `Datastore.FileManagement`.

## Where to assign it

| Object | Role | Propagate |
| --- | --- | --- |
| vCenter root, each datacenter | Read-only | No. Visibility of the path only |
| The datacenter's **root VM folder** | `banlieue-sandbox` | **No** (see below) |
| The dedicated sandbox VM folder (for example `/dc1/vm/sandboxes`), which holds the per-zone template and VM folders | `banlieue-sandbox` | Yes |
| Each target cluster (the root resource pool) | `banlieue-sandbox` | Yes |
| Target datastores or datastore cluster | `banlieue-sandbox` | Yes |
| Allowlisted sandbox port groups, or their DVS | `banlieue-sandbox` | Yes |

Two constraints in today's code make the scope wider than the brief's
"folder and resource pool" target. Both are listed as candidate changes in
the alignment report, not fixed here:

1. **The clone is issued against the datacenter's root VM folder.**
   `clone_vm` passes the root `vmFolder` as the `CloneVM_Task` folder
   argument and puts the real destination only in `location.folder`
   (`vim.rs`, `clone_vm`). vCenter may check
   `VirtualMachine.Inventory.CreateFromExisting` on that root folder
   (unverified). Assigning the role there **without propagation** covers it
   without granting rights over existing VMs below it. If a live test shows
   the check happens only on `location.folder`, drop this row.
2. **There is no resource pool scoping.** `VSphereMachine.spec.resourcePool`
   exists but the controller never sets it and the provider never reads it.
   Every clone and template lands in the **cluster's root pool**, so
   `Resource.AssignVMToPool` must be granted on the cluster. A dedicated
   sandbox resource pool cannot be enforced until that is implemented.

The provider never writes to objects outside these assignments. Anything
the role can reach but banlieue did not create is still exposed to a stolen
credential, which is why the folder, datastores and port groups should be
dedicated to sandboxes.
