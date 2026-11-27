#!/usr/bin/env python3
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
"""Evaluate deploy/admission/provider-connection.yaml offline.

Runs the policy's variables and validations as CEL (cel-python) over
hand-written Provider fixtures, for the classes whose rules are about where a
token may go: ec2 (ADR-0159, ADR-0174 Decisions 5 and 6) and, as a
regression, azure. It proves the expressions, not the API server wiring; the
binding is checked structurally, and crates/banlieue-operator/tests/
admission_policies.rs checks the rules' structure.

`isURL` is a Kubernetes CEL library function that cel-python does not have;
it is stubbed with Python's URL parser (absolute, with a scheme and a host),
which is enough for the fixtures below. An evaluation error counts as a
denial, as failurePolicy: Fail makes it.

The fixtures are HAND-WRITTEN OBJECTS with placeholder IDs.

    pip install cel-python pyyaml        (make provider-connection-policy-test)
    python3 scripts/test-provider-connection-policy.py [--no-policy]

--no-policy evaluates with no policy at all (everything is admitted): the
RED run, which must fail every "rejected" case.
"""
import os
import sys
from urllib.parse import urlparse

import celpy
import celpy.celtypes as celtypes
import yaml

POLICY = os.path.join(os.path.dirname(__file__), "..", "deploy/admission/provider-connection.yaml")

ROLE = "arn:aws:iam::111122223333:role/banlieue-ec2-provider"
EC2 = "https://ec2.ca-central-1.amazonaws.com"
VPCE = "vpce-0123456789abcdef0-abcd1234"
ZERO = "00000000-0000-0000-0000-000000000000"


def ec2(endpoint=EC2, region="ca-central-1", sts=None, block=True, creds=False, cls="ec2"):
    conn = {"endpoint": endpoint}
    if block:
        conn["ec2"] = {"region": region, "roleArn": ROLE}
        if sts is not None:
            conn["ec2"]["stsEndpoint"] = sts
    if creds:
        conn["credentialsRef"] = {"name": "aws-keys"}
    return {"metadata": {}, "spec": {"providerClassRef": {"name": cls}, "connection": conn}}


def azure(endpoint="https://management.azure.com", authority=None):
    block = {"tenantId": ZERO, "clientId": ZERO, "subscriptionId": ZERO,
             "location": "canadacentral", "resourceGroup": "rg"}
    if authority is not None:
        block["authorityHost"] = authority
    return {"metadata": {}, "spec": {"providerClassRef": {"name": "azure"},
                                     "connection": {"endpoint": endpoint, "azure": block}}}


def vsphere(ec2_block=False):
    conn = {"endpoint": "https://vcenter.example.com/sdk", "credentialsRef": {"name": "vc"}}
    if ec2_block:
        conn["ec2"] = {"region": "ca-central-1", "roleArn": ROLE}
    return {"metadata": {}, "spec": {"providerClassRef": {"name": "vsphere"}, "connection": conn}}


# (name, object, admitted?)
CASES = [
    # ---- ec2: class requirements
    ("ec2, regional endpoint", ec2(), True),
    ("ec2, regional endpoint with a trailing slash", ec2(endpoint=EC2 + "/"), True),
    ("ec2 without its block", ec2(block=False), False),
    ("ec2 with a credentialsRef", ec2(creds=True), False),
    ("an ec2 block on vsphere", vsphere(ec2_block=True), False),
    ("vsphere without an ec2 block", vsphere(), True),
    # ---- ec2: the session token's destination
    ("ec2, VPC endpoint", ec2(endpoint=f"https://{VPCE}.ec2.ca-central-1.vpce.amazonaws.com"), True),
    ("ec2, VPC endpoint with a trailing slash", ec2(endpoint=f"https://{VPCE}.ec2.ca-central-1.vpce.amazonaws.com/"), True),
    ("ec2, plain http", ec2(endpoint="http://ec2.ca-central-1.amazonaws.com"), False),
    ("ec2, another region's endpoint", ec2(endpoint="https://ec2.us-east-1.amazonaws.com"), False),
    ("ec2, the global name", ec2(endpoint="https://ec2.amazonaws.com"), False),
    ("ec2, a look-alike suffix", ec2(endpoint="https://ec2.ca-central-1.amazonaws.com.example.com"), False),
    ("ec2, an attacker host", ec2(endpoint="https://ec2.bar.foo.io"), False),
    ("ec2, a path", ec2(endpoint=EC2 + "/x"), False),
    ("ec2, two trailing slashes", ec2(endpoint=EC2 + "//"), False),
    ("ec2, a dot in the region is not a wildcard", ec2(endpoint="https://ec2xca-central-1.amazonaws.com"), False),
    ("ec2, userinfo", ec2(endpoint="https://u@ec2.ca-central-1.amazonaws.com"), False),
    ("ec2, an STS VPC endpoint as the EC2 endpoint", ec2(endpoint=f"https://{VPCE}.sts.ca-central-1.vpce.amazonaws.com"), False),
    ("ec2, a VPC endpoint of another region", ec2(endpoint=f"https://{VPCE}.ec2.us-east-1.vpce.amazonaws.com"), False),
    ("ec2, a short VPC endpoint ID", ec2(endpoint="https://vpce-0123-abcd1234.ec2.ca-central-1.vpce.amazonaws.com"), False),
    ("ec2 in China", ec2(endpoint="https://ec2.cn-north-1.amazonaws.com.cn", region="cn-north-1"), True),
    ("ec2 in China without .cn", ec2(endpoint="https://ec2.cn-north-1.amazonaws.com", region="cn-north-1"), False),
    ("ec2 in GovCloud", ec2(endpoint="https://ec2.us-gov-west-1.amazonaws.com", region="us-gov-west-1"), True),
    # ---- ec2: the ServiceAccount token's destination
    ("ec2, regional STS", ec2(sts="https://sts.ca-central-1.amazonaws.com"), True),
    ("ec2, regional STS with a trailing slash", ec2(sts="https://sts.ca-central-1.amazonaws.com/"), True),
    ("ec2, STS VPC endpoint", ec2(sts=f"https://{VPCE}.sts.ca-central-1.vpce.amazonaws.com"), True),
    ("ec2, global STS", ec2(sts="https://sts.amazonaws.com"), False),
    ("ec2, another region's STS", ec2(sts="https://sts.us-east-1.amazonaws.com"), False),
    ("ec2, an attacker STS", ec2(sts="https://sts.bar.foo.io"), False),
    ("ec2, the EC2 endpoint as STS", ec2(sts=EC2), False),
    ("ec2, plain http STS", ec2(sts="http://sts.ca-central-1.amazonaws.com"), False),
    # ---- azure: unchanged
    ("azure, public cloud", azure(), True),
    ("azure, its own authority", azure(authority="https://login.microsoftonline.com/"), True),
    ("azure, an unknown endpoint", azure(endpoint="https://management.bar.foo.io"), False),
    ("azure, an unpaired authority", azure(authority="https://login.microsoftonline.us"), False),
]


def is_url(value):
    parsed = urlparse(str(value))
    return celtypes.BoolType(bool(parsed.scheme) and bool(parsed.netloc))


def evaluate(expr, activation):
    env = celpy.Environment()
    program = env.program(env.compile(expr), functions={"isURL": is_url})
    return program.evaluate(activation)


def admitted(policy, obj):
    if policy is None:
        return True
    try:
        activation = {"object": celpy.json_to_cel(obj)}
        variables = {}
        for var in policy["spec"].get("variables", []):
            activation["variables"] = celtypes.MapType(variables)
            variables[celtypes.StringType(var["name"])] = evaluate(var["expression"], activation)
        activation["variables"] = celtypes.MapType(variables)
        return all(bool(evaluate(v["expression"], activation)) for v in policy["spec"]["validations"])
    except Exception as exc:  # failurePolicy: Fail denies on an evaluation error
        if "--debug" in sys.argv:
            print(f"     evaluation error: {exc}")
        return False


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
