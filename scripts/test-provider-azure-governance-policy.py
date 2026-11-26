#!/usr/bin/env python3
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
"""Evaluate deploy/admission/provider-azure-governance.yaml offline.

Takes the matchConditions and validations out of the policy and runs them,
as CEL, over hand-written Provider fixtures (ADR-0172). CEL is evaluated by
cel-python, not by an API server, so this proves the expressions, not the
wiring; the wiring (binding, matchConstraints) is checked structurally below.

SCOPE. Case-folded duplicate tag names are not a CEL rule (portable CEL has
no case folding); the provider refuses them (crates/banlieue-azure tags.rs,
crates/banlieue-provider-azure client.rs check_governance). The cases below
cover what the policy does check.

The fixtures are HAND-WRITTEN OBJECTS with placeholder GUIDs.

    pip install cel-python pyyaml        (make azure-governance-policy-test)
    python3 scripts/test-provider-azure-governance-policy.py [--no-policy]

--no-policy evaluates with no policy at all (everything is admitted): the
RED run, which must fail every "rejected" case.
"""
import os
import sys

import celpy
import yaml

POLICY = os.path.join(os.path.dirname(__file__), "..", "deploy/admission/provider-azure-governance.yaml")

SUB = "00000000-0000-0000-0000-000000000000"
OTHER_SUB = "00000000-0000-0000-0000-0000000000ff"


def des(sub=SUB, rg="rg-sec", name="des-1", kind="Microsoft.Compute/diskEncryptionSets"):
    return f"/subscriptions/{sub}/resourceGroups/{rg}/providers/{kind}/{name}"


def prov(tags=None, des_id=None, sub=SUB, azure=True):
    spec = {"providerClassRef": {"name": "azure"}, "connection": {"endpoint": "https://management.azure.com"}}
    if azure:
        block = {"tenantId": SUB, "clientId": SUB, "subscriptionId": sub,
                 "location": "eastus2", "resourceGroup": "rg"}
        if tags is not None:
            block["tags"] = tags
        if des_id is not None:
            block["diskEncryptionSetId"] = des_id
        spec["connection"]["azure"] = block
    return {"spec": spec}


# (name, object, admitted?)
CASES = [
    ("no azure block (policy does not apply)", prov(azure=False), True),
    ("azure block, nothing set", prov(), True),
    ("extra tags", prov(tags={"cost-centre": "1234", "env": "prod"}), True),
    ("an empty tag value", prov(tags={"cost-centre": ""}), True),
    ("45 tags", prov(tags={f"t{i}": "x" for i in range(45)}), True),
    ("46 tags", prov(tags={f"t{i}": "x" for i in range(46)}), False),
    ("an ownership tag", prov(tags={"banlieue-owner-uid": "x"}), False),
    ("an ownership tag in another case", prov(tags={"Banlieue-Owner-Uid": "x"}), False),
    ("the digest tag", prov(tags={"BANLIEUE-SOURCE-SHA256": "00"}), False),
    ("banlieue without a dash is fine", prov(tags={"banlieue": "x"}), True),
    ("a slash in a name", prov(tags={"cost/centre": "1"}), False),
    ("a backslash in a name", prov(tags={"cost\\centre": "1"}), False),
    ("a question mark in a name", prov(tags={"cost?": "1"}), False),
    ("an empty name", prov(tags={"": "1"}), False),
    ("a 512-character name", prov(tags={"n" * 512: "1"}), True),
    ("a 513-character name", prov(tags={"n" * 513: "1"}), False),
    ("a 257-character value", prov(tags={"n": "v" * 257}), False),
    ("a disk encryption set", prov(des_id=des()), True),
    ("a disk encryption set, other casing", prov(des_id=des().replace("resourceGroups", "resourcegroups").replace("Microsoft.Compute", "microsoft.compute")), True),
    ("a set in another subscription", prov(des_id=des(sub=OTHER_SUB)), False),
    ("a Key Vault instead of a set", prov(des_id=des(kind="Microsoft.KeyVault/vaults")), False),
    ("a bare name", prov(des_id="des-1"), False),
    ("a trailing segment", prov(des_id=des() + "/extra"), False),
    ("a query string", prov(des_id=des() + "?x=1"), False),
]


def evaluate(expr, obj):
    env = celpy.Environment()
    return env.program(env.compile(expr)).evaluate({"object": celpy.json_to_cel(obj)})


def admitted(policy, obj):
    if policy is None:
        return True
    for mc in policy["spec"].get("matchConditions", []):
        if not bool(evaluate(mc["expression"], obj)):
            return True  # policy does not apply
    return all(bool(evaluate(v["expression"], obj)) for v in policy["spec"]["validations"])


def structural(docs):
    pol = [d for d in docs if d["kind"] == "ValidatingAdmissionPolicy"]
    bind = [d for d in docs if d["kind"] == "ValidatingAdmissionPolicyBinding"]
    assert len(pol) == 1 and len(bind) == 1, "one policy, one binding"
    assert bind[0]["spec"]["policyName"] == pol[0]["metadata"]["name"]
    assert bind[0]["spec"]["validationActions"] == ["Deny"]
    assert pol[0]["spec"]["failurePolicy"] == "Fail"
    rule = pol[0]["spec"]["matchConstraints"]["resourceRules"][0]
    assert rule["resources"] == ["providers"] and rule["operations"] == ["CREATE", "UPDATE"]
    return pol[0]


def main():
    no_policy = "--no-policy" in sys.argv
    with open(POLICY) as f:
        policy = structural(list(yaml.safe_load_all(f)))
    policy = None if no_policy else policy
    bad = 0
    for name, obj, want in CASES:
        got = admitted(policy, obj)
        ok = got == want
        bad += not ok
        print(f"{'ok  ' if ok else 'FAIL'} {'admit ' if want else 'reject'} {name}")
    print(f"{len(CASES) - bad}/{len(CASES)} passed" + (" (no policy)" if no_policy else ""))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
