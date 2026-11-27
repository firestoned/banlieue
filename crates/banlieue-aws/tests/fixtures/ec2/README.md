<!--
Copyright (c) 2026 Erick Bourgeois, banlieue
SPDX-License-Identifier: Apache-2.0
-->
# EC2, STS and S3 response fixtures

Loaded by `src/types_tests.rs` with `include_str!`. Each file is either a
sample response copied from AWS's own API reference, or, where the reference
has no sample, a minimal response built from the documented element names.
Every source was read on 2026-11-26.

## Copied from the AWS reference

| File | Source | Changes |
| --- | --- | --- |
| `run-instances.xml` | [RunInstances](https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_RunInstances.html), Example 15, sample response | The page's `<instanceType><instanceType></instanceType>` placeholder is not well-formed XML; it reads `m5.large` here. The secondary interfaces' `11.0.N.251` addresses (routable) are replaced with RFC 5737 `198.51.100.N`. |
| `describe-instances.xml` | [DescribeInstances](https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_DescribeInstances.html), Example 2 | The public address `54.194.252.215` (and its DNS name) is replaced with RFC 5737 `203.0.113.215`. |
| `terminate-instances.xml` | [TerminateInstances](https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_TerminateInstances.html), Example | none |
| `stop-instances.xml` | [StopInstances](https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_StopInstances.html), Example | none |
| `start-instances.xml` | [StartInstances](https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_StartInstances.html), Example | none |
| `get-console-output.xml` | [GetConsoleOutput](https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_GetConsoleOutput.html), Example 1 | none (the base64 is wrapped over lines, as published) |
| `describe-availability-zones.xml` | [DescribeAvailabilityZones](https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_DescribeAvailabilityZones.html), Example | none |
| `error-ec2.xml` | [Error codes for the Amazon EC2 API](https://docs.aws.amazon.com/AWSEC2/latest/APIReference/errors-overview.html), "Example error response" | none (note `RequestID`, EC2's spelling) |

## Built from the documented element names

| File | Why built | Element names from |
| --- | --- | --- |
| `describe-instance-type-offerings.xml` | The page has no example. | [DescribeInstanceTypeOfferings](https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_DescribeInstanceTypeOfferings.html) (`instanceTypeOfferingSet`), [InstanceTypeOffering](https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_InstanceTypeOffering.html) (`instanceType`, `locationType`, `location`) |
| `describe-instance-types.xml` | The page has no example, so none with NitroTPM. | [DescribeInstanceTypes](https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_DescribeInstanceTypes.html) (`instanceTypeSet`), [InstanceTypeInfo](https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_InstanceTypeInfo.html), [ProcessorInfo](https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_ProcessorInfo.html), [VCpuInfo](https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_VCpuInfo.html), [NitroTpmInfo](https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_NitroTpmInfo.html). The reference lists list members in their request form (`SupportedArchitectures.N`, `SupportedBootModes.N`, `SupportedVersions.N`); the response elements are assumed to be the lower-camel names wrapping `<item>`s (`supportedArchitectures`, `supportedBootModes`, `supportedVersions`), as every other EC2 list is. To be confirmed against a live response. |
| `get-instance-tpm-ek-pub-rsa.xml`, `get-instance-tpm-ek-pub-ecc.xml` | The page has no example. | [GetInstanceTpmEkPub](https://docs.aws.amazon.com/AWSEC2/latest/APIReference/API_GetInstanceTpmEkPub.html) (`instanceId`, `keyType`, `keyFormat`, `keyValue`). The keys are throwaway RSA-2048 and P-384 keys generated with `openssl genpkey` for this fixture (private halves discarded), not a NitroTPM's. |
| `ek-rsa-2048.pem`, `ek-ecc-p384.pem` | Expected output of `to_pem_spki` | `openssl pkey -pubout` of the same throwaway keys. |
| `error-sts.xml` | The STS reference lists codes but shows no body. | The Query protocol's `<ErrorResponse><Error><Type><Code><Message></Error><RequestId>` shape ([STS common errors](https://docs.aws.amazon.com/STS/latest/APIReference/CommonErrors.html) for the codes). |
| `error-s3.xml` | The S3 error page did not render for fetching. | [S3 error responses](https://docs.aws.amazon.com/AmazonS3/latest/API/ErrorResponses.html): `<Error><Code><Message><Resource><RequestId>`. |
