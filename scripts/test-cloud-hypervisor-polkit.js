// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//
// Evaluates deploy/provider-cloud-hypervisor/host/60-banlieue-cloud-hypervisor.rules
// against a stub polkit, with the placeholders the bootstrap fills in, and
// asserts exactly what the provider's user may and may not do.
//
//   node scripts/test-cloud-hypervisor-polkit.js     (or: make ch-polkit-test)
"use strict";
const fs = require("fs");
const path = require("path");

const rulesPath = path.join(__dirname, "..", "deploy/provider-cloud-hypervisor/host/60-banlieue-cloud-hypervisor.rules");
const src = fs.readFileSync(rulesPath, "utf8")
  .replace(/@BANLIEUE_USER@/g, "banlieue")
  .replace(/@GUEST_UID_BASE@/g, "2000000")
  .replace(/@GUEST_UID_COUNT@/g, "1024");
if (/@[A-Z_]+@/.test(src)) throw new Error("unreplaced placeholder");

let rule;
const polkit = {
  Result: { YES: "yes", NO: "no", NOT_HANDLED: "not_handled" },
  addRule(f) { rule = f; },
};
new Function("polkit", src)(polkit);

function decide(user, unit, verb, id = "org.freedesktop.systemd1.manage-units") {
  const action = { id, lookup: (k) => ({ unit, verb })[k] };
  return rule(action, { user });
}

const cases = [
  // allowed
  ["banlieue", "banlieue-ch@2000000.service", "start", "yes"],
  ["banlieue", "banlieue-ch@2001023.service", "stop", "yes"],
  ["banlieue", "banlieue-swtpm@2000007.service", "reset-failed", "yes"],
  ["banlieue", "banlieue-swtpm-setup@2000007.service", "start", "yes"],
  ["banlieue", "banlieue-ch@2000007.service", "set-property", "yes"],
  ["banlieue", "banlieue-ch-import@6f1c2d3e-4a5b-4c6d-8e7f-0a1b2c3d4e5f.service", "start", "yes"],
  // outside the guest range: root, the provider itself, the next uid
  ["banlieue", "banlieue-ch@0.service", "start", "not_handled"],
  ["banlieue", "banlieue-ch@999.service", "start", "not_handled"],
  ["banlieue", "banlieue-swtpm@2001024.service", "start", "not_handled"],
  ["banlieue", "banlieue-ch@1999999.service", "start", "not_handled"],
  // not a plain decimal instance
  ["banlieue", "banlieue-ch@-1.service", "start", "not_handled"],
  ["banlieue", "banlieue-ch@2000000x.service", "start", "not_handled"],
  ["banlieue", "banlieue-ch@root.service", "start", "not_handled"],
  // the old transient names, and anything else
  ["banlieue", "banlieue-ch-6f1c2d3e-4a5b-4c6d-8e7f-0a1b2c3d4e5f.service", "start", "not_handled"],
  ["banlieue", "ssh.service", "stop", "not_handled"],
  ["banlieue", "banlieue-provider-cloud-hypervisor.service", "stop", "not_handled"],
  // set-property only on VMM instances
  ["banlieue", "banlieue-swtpm@2000007.service", "set-property", "not_handled"],
  ["banlieue", "banlieue-ch-import@6f1c2d3e-4a5b-4c6d-8e7f-0a1b2c3d4e5f.service", "set-property", "not_handled"],
  // other verbs
  ["banlieue", "banlieue-ch@2000007.service", "enable", "not_handled"],
  ["banlieue", "banlieue-ch@2000007.service", "restart", "not_handled"],
  // anyone else
  ["mallory", "banlieue-ch@2000007.service", "start", "not_handled"],
];

let failed = 0;
for (const [user, unit, verb, want] of cases) {
  const got = decide(user, unit, verb);
  if (got !== want) {
    failed++;
    console.error(`FAIL ${user} ${verb} ${unit}: got ${got}, want ${want}`);
  }
}
if (decide("banlieue", "banlieue-ch@2000007.service", "start", "org.freedesktop.login1.power-off") !== "not_handled") {
  failed++;
  console.error("FAIL: another action id was handled");
}
console.log(`${cases.length + 1 - failed}/${cases.length + 1} polkit cases passed`);
process.exit(failed ? 1 : 0);
