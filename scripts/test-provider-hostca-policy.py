#!/usr/bin/env python3
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
"""Evaluate deploy/admission/provider-attestation-hostca.yaml offline.

Takes the matchConditions and validations out of the policy and runs them,
as CEL, over hand-written Provider fixtures. CEL is evaluated by cel-python,
not by an API server, so this proves the expressions, not the wiring; the
wiring (binding, matchConstraints) is checked structurally below.

SCOPE. The policy checks only the "exactly one source" shape. "An inline PEM
holds exactly one certificate" and "no private key" are not CEL rules here:
they stay in ProviderAttestation::validate (crates/banlieue-api), which runs in
the controller, after the object is stored. So the cases below cover the shape
only. A stricter CEL version of those two rules was written and passed 9 more
fixtures offline (branch feat/firecracker-hostca-admission); it was not taken.

The fixtures are HAND-WRITTEN OBJECTS, not Provider manifests.

    pip install cel-python pyyaml        (make hostca-policy-test)
    python3 scripts/test-provider-hostca-policy.py [--no-policy]

--no-policy evaluates with no policy at all (everything is admitted): the
RED run, which must fail every "rejected" case.
"""
import os
import sys

import celpy
import yaml

POLICY = os.path.join(os.path.dirname(__file__), "..", "deploy/admission/provider-attestation-hostca.yaml")

CERT = "placeholder certificate text; the policy does not read it"

CM = {"name": "host-ca", "key": "ca.crt"}


def prov(host_ca=None, attestation=True):
    spec = {"providerClassRef": {"name": "firecracker"}}
    if attestation:
        spec["attestation"] = {} if host_ca is None else {"hostCa": host_ca}
    return {"spec": spec}


# (name, object, admitted?)
CASES = [
    ("inline single certificate", prov({"inline": CERT}), True),
    ("configMapRef alone", prov({"configMapRef": CM}), True),
    ("secretRef alone", prov({"secretRef": CM}), True),
    ("no hostCa (absence is valid)", prov(), True),
    ("no attestation block (absence is valid)", prov(attestation=False), True),
    ("no source at all", prov({}), False),
    ("two sources", prov({"inline": CERT, "configMapRef": CM}), False),
    ("three sources", prov({"inline": CERT, "configMapRef": CM, "secretRef": CM}), False),
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
