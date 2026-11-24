<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# ARM response fixtures

Real Azure Resource Manager responses captured during the roadmap 23 phase 0
spike (2026-11-24), sanitized: every GUID (subscription, tenant, resource and
principal IDs) is replaced by `00000000-0000-0000-0000-00000000000N`, MAC
addresses are zeroed, and account-identifying resource names are replaced by
`banlieue-example*`. The private addresses are the spike's own RFC 1918 test
subnet.

| File | Call | Captured or written |
| --- | --- | --- |
| `instance-view-running.json` | `GET .../virtualMachines/{vm}/instanceView` | captured (a Debian VM with the Azure agent) |
| `instance-view-deallocated.json` | same | captured (a banlieue Kairos VM, deallocated) |
| `instance-view-provisioning-timed-out.json` | same | **written** from the status codes and message the spike recorded on four VMs that timed out; those VMs no longer exist |
| `nic.json` | `GET .../networkInterfaces/{nic}` | captured |
| `resource-skus.json` | `GET /subscriptions/{s}/providers/Microsoft.Compute/skus` | captured, trimmed to four sizes and five capabilities |
| `usages.json` | `GET .../locations/{region}/usages` | captured, trimmed to five entries |
| `resources.json` | `GET .../resourceGroups/{rg}/resources?$expand=createdTime,changedTime` | captured |

Two facts the fixtures carry on purpose:

- ARM returned one resource group name **upper-cased** inside disk IDs
  (`BANLIEUE-EXAMPLE`); resource IDs are compared case-insensitively.
- Usage names and SKU `family` values use irregular casing
  (`StandardDsv7Family`, `standardDav6Family`); they are matched
  case-insensitively.
