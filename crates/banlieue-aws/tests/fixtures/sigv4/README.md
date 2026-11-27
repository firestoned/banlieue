# SigV4 test vectors

Read by `crates/banlieue-aws/src/sigv4_tests.rs`. Every credential here is a
public AWS documentation constant, not a real key.

## `aws-c-auth/`

AWS's SigV4 test suite, copied unchanged from
<https://github.com/awslabs/aws-c-auth/tree/main/tests/aws-signing-test-suite/v4>
at commit `f556deacd6e6d9dbf540ff00a9bf383688076874`, fetched 2026-11-26.
Only the header-signing files are kept (`context.json`, `request.txt`,
`header-canonical-request.txt`, `header-string-to-sign.txt`,
`header-signature.txt`, `header-signed-request.txt`); the presigned-URL
(`query-*`) files are not used.

Two cases of the suite are left out on purpose:

- `get-header-value-multiline`: obsolete HTTP line folding, which no client
  banlieue uses can send;
- `post-sts-header-after`: the session token added after signing, unsigned.
  banlieue always signs `x-amz-security-token` (`post-sts-header-before` and
  `get-vanilla-with-session-token` cover it).

The suite's request lines carry the path as it goes on the wire, unencoded
(`/example space/`), and aws-c-auth encodes it once more for the canonical
request. The tests therefore sign the path with one encoding and the
`normalize` flag from `context.json`.

## `s3-docs/`

The four worked examples of the Amazon S3 API Reference page "Signature
Calculations for the Authorization Header: Transferring Payload in a Single
Chunk", <https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-header-based-auth.html>,
as archived on 2023-01-11
(<https://web.archive.org/web/20230111084644/https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-header-based-auth.html>;
the live page no longer carries them). Transcribed into the aws-c-auth file
layout: the canonical requests were rebuilt from the page and each hashes to
the page's string-to-sign; the signatures are the page's. They exercise the
S3 style (single path encoding, no normalization, `x-amz-content-sha256`
signed).
