#!/usr/bin/env python3
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
"""Evaluate deploy/admission/provider-attestation-ektrustbundle.yaml offline.

Runs the policy's matchConditions and validations as CEL (cel-python) over
hand-written Provider fixtures. It proves the expressions, not the API server
wiring; the binding is checked structurally.

Why this exists: ADR-0143 makes spec.attestation.ekTrustBundle optional (a
firecracker Provider carries hostCa and no EK bundle). The policy used to read
ekTrustBundle whenever an attestation block was present, so a hostCa-only
Provider hit a "no such key" evaluation error, which failurePolicy: Fail turns
into a denial. An evaluation error is therefore treated as a denial here too.

    pip install cel-python pyyaml        (make ektrustbundle-policy-test)
    python3 scripts/test-provider-ektrustbundle-policy.py [--no-policy]
"""
import os
import sys

import celpy
import yaml

POLICY = os.path.join(
    os.path.dirname(__file__), "..", "deploy/admission/provider-attestation-ektrustbundle.yaml"
)

CM = {"name": "ek-cas", "key": "ca.crt"}


def prov(attestation):
    spec = {"providerClassRef": {"name": "cloud-hypervisor"}}
    if attestation is not None:
        spec["attestation"] = attestation
    return {"spec": spec}


# (name, object, admitted?)
CASES = [
    ("inline alone", prov({"ekTrustBundle": {"inline": "pem"}}), True),
    ("configMapRef alone", prov({"ekTrustBundle": {"configMapRef": CM}}), True),
    ("secretRef alone", prov({"ekTrustBundle": {"secretRef": CM}}), True),
    ("no attestation block", prov(None), True),
    ("hostCa only, no ekTrustBundle (ADR-0143)", prov({"hostCa": {"inline": "pem"}}), True),
    ("empty attestation block", prov({}), True),
    ("hostCa and a valid ekTrustBundle", prov({"hostCa": {"inline": "pem"}, "ekTrustBundle": {"inline": "pem"}}), True),
    ("ekTrustBundle with no source", prov({"ekTrustBundle": {}}), False),
    ("two sources", prov({"ekTrustBundle": {"inline": "pem", "configMapRef": CM}}), False),
    ("three sources", prov({"ekTrustBundle": {"inline": "pem", "configMapRef": CM, "secretRef": CM}}), False),
    ("hostCa present, ekTrustBundle with two sources", prov({"hostCa": {"inline": "pem"}, "ekTrustBundle": {"inline": "pem", "secretRef": CM}}), False),
]


def evaluate(expr, obj):
    env = celpy.Environment()
    return env.program(env.compile(expr)).evaluate({"object": celpy.json_to_cel(obj)})


def admitted(policy, obj):
    if policy is None:
        return True
    try:
        for mc in policy["spec"].get("matchConditions", []):
            if not bool(evaluate(mc["expression"], obj)):
                return True  # policy does not apply
        return all(bool(evaluate(v["expression"], obj)) for v in policy["spec"]["validations"])
    except Exception:
        return False  # failurePolicy: Fail denies on an evaluation error


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
