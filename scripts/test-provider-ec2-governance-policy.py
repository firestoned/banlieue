#!/usr/bin/env python3
# Copyright (c) 2026 Erick Bourgeois, banlieue
# SPDX-License-Identifier: Apache-2.0
"""Evaluate deploy/admission/provider-ec2-governance.yaml offline.

Takes the matchConditions and validations out of the policy and runs them,
as CEL, over hand-written Provider fixtures (ADR-0174, the EC2 counterpart of
ADR-0172's extra tags). CEL is evaluated by cel-python, not by an API server,
so this proves the expressions, not the wiring; the wiring (binding,
matchConstraints) is checked structurally below.

The same rules are the Rust check banlieue_api::aws::validate_extra_tags
(crates/banlieue-api/src/aws.rs), tested over the same kinds of case.

The fixtures are HAND-WRITTEN OBJECTS with a placeholder account.

    pip install cel-python pyyaml        (make ec2-governance-policy-test)
    python3 scripts/test-provider-ec2-governance-policy.py [--no-policy]

--no-policy evaluates with no policy at all (everything is admitted): the
RED run, which must fail every "rejected" case.
"""
import os
import sys

import celpy
import yaml

POLICY = os.path.join(os.path.dirname(__file__), "..", "deploy/admission/provider-ec2-governance.yaml")

ROLE = "arn:aws:iam::111122223333:role/banlieue-ec2-provider"


def prov(tags=None, ec2=True):
    spec = {"providerClassRef": {"name": "ec2"},
            "connection": {"endpoint": "https://ec2.ca-central-1.amazonaws.com"}}
    if ec2:
        block = {"region": "ca-central-1", "roleArn": ROLE}
        if tags is not None:
            block["tags"] = tags
        spec["connection"]["ec2"] = block
    return {"spec": spec}


# (name, object, admitted?)
CASES = [
    ("no ec2 block (policy does not apply)", prov(ec2=False), True),
    ("ec2 block, no tags", prov(), True),
    ("extra tags", prov(tags={"cost-centre": "1234", "env": "prod"}), True),
    ("every allowed character", prov(tags={"a Z 0 + = . _ : / @ -": "a Z 0 + = . _ : / @ -"}), True),
    ("an empty value", prov(tags={"cost-centre": ""}), True),
    ("44 tags", prov(tags={f"t{i}": "x" for i in range(44)}), True),
    ("45 tags", prov(tags={f"t{i}": "x" for i in range(45)}), False),
    ("an ownership tag", prov(tags={"banlieue-owner-uid": "x"}), False),
    ("an ownership tag in another case", prov(tags={"Banlieue-Owner-Uid": "x"}), False),
    ("the digest tag", prov(tags={"BANLIEUE-SOURCE-SHA256": "00"}), False),
    ("banlieue without a dash is fine", prov(tags={"banlieue": "x"}), True),
    ("an aws: tag", prov(tags={"aws:cloudformation:stack-name": "x"}), False),
    ("an aws: tag in another case", prov(tags={"AWS:x": "x"}), False),
    ("aws without a colon is fine", prov(tags={"aws-account": "x"}), True),
    ("Name", prov(tags={"Name": "x"}), False),
    ("name in lowercase is another key", prov(tags={"name": "x"}), True),
    ("an empty key", prov(tags={"": "1"}), False),
    ("a 128-character key", prov(tags={"k" * 128: "1"}), True),
    ("a 129-character key", prov(tags={"k" * 129: "1"}), False),
    ("a 256-character value", prov(tags={"k": "v" * 256}), True),
    ("a 257-character value", prov(tags={"k": "v" * 257}), False),
    ("a semicolon in a key", prov(tags={"a;b": "1"}), False),
    ("a comma in a value", prov(tags={"k": "a,b"}), False),
    ("a tab in a value", prov(tags={"k": "a\tb"}), False),
    ("a non-ASCII letter in a key", prov(tags={"café": "1"}), False),
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
