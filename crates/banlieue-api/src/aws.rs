// Copyright (c) 2026 Erick Bourgeois, banlieue
// SPDX-License-Identifier: Apache-2.0
//! Shapes of the AWS identifiers the `ec2` provider class carries
//! (ADR-0174, ADR-0162 Decision 9).
//!
//! Each shape is written twice, on purpose: as a regular expression the
//! generated CRD schema enforces when the object is stored, and as a Rust
//! function the provider checks before any AWS call. A unit test runs both
//! over the same cases so they cannot drift. Where a rule cannot be said as a
//! regular expression (an S3 bucket's reserved prefixes, the tag rules that
//! span a whole map), the Rust check is the stricter one and the admission
//! policy `deploy/admission/provider-ec2-governance.yaml` carries what CEL
//! can.
//!
//! Only the `aws`, `aws-cn` and `aws-us-gov` partitions are accepted: their
//! endpoints are `amazonaws.com` and `amazonaws.com.cn` names, the only ones
//! the admission policy lets a session token reach.

use std::collections::BTreeMap;

/// Region: two letters, an optional `-gov`, a direction or area word and a
/// number (`ca-central-1`, `us-gov-west-1`, `cn-north-1`).
pub const REGION_PATTERN: &str = r"^[a-z]{2}(-gov)?-[a-z]+-[0-9]{1,2}$";
/// An AMI ID, short (8 hex) or long (17 hex) form.
pub const AMI_ID_PATTERN: &str = r"^ami-([0-9a-f]{8}|[0-9a-f]{17})$";
/// A subnet ID.
pub const SUBNET_ID_PATTERN: &str = r"^subnet-([0-9a-f]{8}|[0-9a-f]{17})$";
/// A security group ID.
pub const SECURITY_GROUP_ID_PATTERN: &str = r"^sg-([0-9a-f]{8}|[0-9a-f]{17})$";
/// An IAM role ARN, `arn:<partition>:iam::<account>:role/<path/><name>`. The
/// path (`[!-~]`, at most 512 characters with its slashes) is optional.
pub const IAM_ROLE_ARN_PATTERN: &str =
    r"^arn:aws(-cn|-us-gov)?:iam::[0-9]{12}:role/([!-~]{0,510}/)?[A-Za-z0-9+=,.@_-]{1,64}$";
/// An IAM role name, without path.
pub const IAM_ROLE_NAME_PATTERN: &str = r"^[A-Za-z0-9+=,.@_-]{1,64}$";
/// An S3 bucket name banlieue accepts: S3's rules without dots (a dotted
/// name breaks the wildcard certificate of virtual-hosted-style HTTPS).
pub const S3_BUCKET_NAME_PATTERN: &str = r"^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$";
/// A KMS key: key ID, key ARN, alias name or alias ARN.
pub const KMS_KEY_PATTERN: &str = r"^((arn:aws(-cn|-us-gov)?:kms:[a-z0-9-]+:[0-9]{12}:key/)?([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|mrk-[0-9a-f]{32})|(arn:aws(-cn|-us-gov)?:kms:[a-z0-9-]+:[0-9]{12}:)?alias/[A-Za-z0-9/_-]{1,250})$";
/// The characters banlieue accepts in an extra tag's key or value: ASCII
/// letters and digits, space, and `+ - = . _ : / @`. EC2 also accepts
/// non-ASCII letters; banlieue does not, so the admission policy and the
/// provider agree on every engine.
pub const TAG_CHARACTERS_PATTERN: &str = r"^[a-zA-Z0-9 +=._:/@-]*$";

/// Digits in an AWS account ID.
pub const ACCOUNT_ID_DIGITS: usize = 12;
/// Hex digits in a short-form EC2 resource ID (`ami-12345678`).
pub const SHORT_RESOURCE_ID_HEX: usize = 8;
/// Hex digits in a long-form EC2 resource ID (`ami-0123456789abcdef0`).
pub const LONG_RESOURCE_ID_HEX: usize = 17;
/// Longest IAM role name.
pub const MAX_ROLE_NAME_LEN: usize = 64;
/// Longest IAM path between `role/` and the name, excluding the leading
/// slash (IAM allows 512 characters with both slashes).
pub const MAX_ROLE_PATH_LEN: usize = 510;
/// Shortest S3 bucket name.
pub const MIN_BUCKET_NAME_LEN: usize = 3;
/// Longest S3 bucket name.
pub const MAX_BUCKET_NAME_LEN: usize = 63;
/// Longest KMS alias after `alias/`.
pub const MAX_KMS_ALIAS_LEN: usize = 250;
/// Longest EC2 tag key.
pub const MAX_TAG_KEY_LEN: usize = 128;
/// Longest EC2 tag value.
pub const MAX_TAG_VALUE_LEN: usize = 256;
/// Most extra tags on one `Provider`: EC2 allows 50 per resource, and
/// banlieue keeps six for its own (`Name` and the ownership and source
/// digest tags of ADR-0158 and ADR-0162).
pub const MAX_EXTRA_TAGS: usize = 44;

/// Prefix of every tag banlieue writes itself, refused in any case.
const BANLIEUE_TAG_PREFIX: &str = "banlieue-";
/// Prefix AWS reserves for its own tags, refused in any case.
const AWS_TAG_PREFIX: &str = "aws:";
/// The tag banlieue sets to the machine's name (ADR-0174 Decision 3).
const NAME_TAG: &str = "Name";

/// Hex digits in a KMS multi-region key ID after `mrk-`.
const MRK_HEX: usize = 32;
/// Lengths of the five groups of a UUID.
const UUID_GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
/// Lowercase alphanumerics after a VPC endpoint ID in its DNS name.
const VPCE_SUFFIX_LEN: usize = 8;
/// Longest region number (`eu-central-12`).
const MAX_REGION_NUMBER_DIGITS: usize = 2;
/// Letters in a region's leading area code.
const REGION_AREA_LETTERS: usize = 2;

/// Bucket name prefixes S3 reserves.
const RESERVED_BUCKET_PREFIXES: [&str; 3] = ["xn--", "sthree-", "amzn-s3-demo-"];
/// Bucket name suffixes S3 reserves.
const RESERVED_BUCKET_SUFFIXES: [&str; 4] = ["-s3alias", "--ol-s3", "--x-s3", "--table-s3"];

/// The partitions banlieue accepts in an ARN.
const PARTITIONS: [&str; 3] = ["aws", "aws-cn", "aws-us-gov"];

fn is_lower_hex(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_digits(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit())
}

/// Whether `s` is `<prefix>-` and 8 or 17 lowercase hex digits.
fn is_resource_id(prefix: &str, s: &str) -> bool {
    s.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|hex| {
            (hex.len() == SHORT_RESOURCE_ID_HEX || hex.len() == LONG_RESOURCE_ID_HEX)
                && is_lower_hex(hex)
        })
}

/// Whether `s` has [`REGION_PATTERN`]'s shape.
#[must_use]
pub fn is_region(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    let Some((number, words)) = parts.split_last() else {
        return false;
    };
    let Some((area, rest)) = words.split_first() else {
        return false;
    };
    let rest = match rest {
        ["gov", tail @ ..] => tail,
        other => other,
    };
    area.len() == REGION_AREA_LETTERS
        && area.bytes().all(|b| b.is_ascii_lowercase())
        && rest.len() == 1
        && rest
            .iter()
            .all(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase()))
        && !number.is_empty()
        && number.len() <= MAX_REGION_NUMBER_DIGITS
        && number.bytes().all(|b| b.is_ascii_digit())
}

/// The ARN partition a region belongs to: `aws-cn` for `cn-*`, `aws-us-gov`
/// for `us-gov-*`, `aws` otherwise.
#[must_use]
pub fn partition_of_region(region: &str) -> &'static str {
    if region.starts_with("cn-") {
        return "aws-cn";
    }
    if region.starts_with("us-gov-") {
        return "aws-us-gov";
    }
    "aws"
}

/// The DNS suffix of a region's service endpoints.
#[must_use]
pub fn dns_suffix_of_region(region: &str) -> &'static str {
    if region.starts_with("cn-") {
        return "amazonaws.com.cn";
    }
    "amazonaws.com"
}

/// Whether `s` is an AMI ID ([`AMI_ID_PATTERN`]).
#[must_use]
pub fn is_ami_id(s: &str) -> bool {
    is_resource_id("ami", s)
}

/// Whether `s` is a subnet ID ([`SUBNET_ID_PATTERN`]).
#[must_use]
pub fn is_subnet_id(s: &str) -> bool {
    is_resource_id("subnet", s)
}

/// Whether `s` is a security group ID ([`SECURITY_GROUP_ID_PATTERN`]).
#[must_use]
pub fn is_security_group_id(s: &str) -> bool {
    is_resource_id("sg", s)
}

/// Whether `s` is an instance ID (`i-` and 8 or 17 hex digits).
#[must_use]
pub fn is_instance_id(s: &str) -> bool {
    is_resource_id("i", s)
}

fn is_role_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"+=,.@_-".contains(&b)
}

/// Whether `s` is an IAM role name ([`IAM_ROLE_NAME_PATTERN`]).
#[must_use]
pub fn is_iam_role_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_ROLE_NAME_LEN && s.bytes().all(is_role_name_byte)
}

/// The fields of `arn:<partition>:<service>:<region>:<account>:<resource>`,
/// when the partition is one banlieue accepts.
fn arn_fields(arn: &str) -> Option<[&str; 5]> {
    let rest = arn.strip_prefix("arn:")?;
    let mut it = rest.splitn(5, ':');
    let fields = [it.next()?, it.next()?, it.next()?, it.next()?, it.next()?];
    PARTITIONS.contains(&fields[0]).then_some(fields)
}

/// The partition of an ARN, when it is one banlieue accepts.
#[must_use]
pub fn partition_of_arn(arn: &str) -> Option<&str> {
    arn_fields(arn).map(|f| f[0])
}

/// Whether `s` is an IAM role ARN ([`IAM_ROLE_ARN_PATTERN`]).
#[must_use]
pub fn is_iam_role_arn(s: &str) -> bool {
    let Some([_, service, region, account, resource]) = arn_fields(s) else {
        return false;
    };
    if service != "iam" || !region.is_empty() || !is_digits(account, ACCOUNT_ID_DIGITS) {
        return false;
    }
    let Some(path_and_name) = resource.strip_prefix("role/") else {
        return false;
    };
    let (path, name) = path_and_name
        .rsplit_once('/')
        .unwrap_or(("", path_and_name));
    path.len() <= MAX_ROLE_PATH_LEN
        && path.bytes().all(|b| (b'!'..=b'~').contains(&b))
        && is_iam_role_name(name)
}

/// The account of an IAM role ARN, when `arn` is one.
#[must_use]
pub fn account_of_role_arn(arn: &str) -> Option<&str> {
    if !is_iam_role_arn(arn) {
        return None;
    }
    arn_fields(arn).map(|f| f[3])
}

/// Whether `s` is an S3 bucket name banlieue accepts
/// ([`S3_BUCKET_NAME_PATTERN`], and none of S3's reserved prefixes or
/// suffixes).
#[must_use]
pub fn is_s3_bucket_name(s: &str) -> bool {
    let bytes = s.as_bytes();
    let (Some(first), Some(last)) = (bytes.first(), bytes.last()) else {
        return false;
    };
    let edge = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    (MIN_BUCKET_NAME_LEN..=MAX_BUCKET_NAME_LEN).contains(&s.len())
        && edge(first)
        && edge(last)
        && bytes.iter().all(|b| edge(b) || *b == b'-')
        && !RESERVED_BUCKET_PREFIXES.iter().any(|p| s.starts_with(p))
        && !RESERVED_BUCKET_SUFFIXES.iter().any(|p| s.ends_with(p))
}

fn is_uuid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == UUID_GROUPS.len()
        && groups
            .iter()
            .zip(UUID_GROUPS)
            .all(|(g, len)| g.len() == len && is_lower_hex(g))
}

fn is_kms_key_id(s: &str) -> bool {
    is_uuid(s)
        || s.strip_prefix("mrk-")
            .is_some_and(|hex| hex.len() == MRK_HEX && is_lower_hex(hex))
}

fn is_kms_alias(s: &str) -> bool {
    s.strip_prefix("alias/").is_some_and(|name| {
        !name.is_empty()
            && name.len() <= MAX_KMS_ALIAS_LEN
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/_-".contains(&b))
    })
}

/// Whether `s` names a KMS key ([`KMS_KEY_PATTERN`]): a key ID, a key ARN,
/// an alias name or an alias ARN.
#[must_use]
pub fn is_kms_key(s: &str) -> bool {
    let Some([_, service, region, account, resource]) = arn_fields(s) else {
        return is_kms_key_id(s) || is_kms_alias(s);
    };
    service == "kms"
        && !region.is_empty()
        && region
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && is_digits(account, ACCOUNT_ID_DIGITS)
        && (resource.strip_prefix("key/").is_some_and(is_kms_key_id) || is_kms_alias(resource))
}

/// `https://<service>.<region>.<suffix>`, the regional endpoint of an AWS
/// service.
#[must_use]
pub fn regional_endpoint(service: &str, region: &str) -> String {
    format!(
        "https://{service}.{region}.{}",
        dns_suffix_of_region(region)
    )
}

/// Whether `host` is `vpce-<17 hex>-<8 lowercase alphanumerics>`, the
/// regional DNS label of an interface VPC endpoint.
fn is_vpce_label(host: &str) -> bool {
    let Some(rest) = host.strip_prefix("vpce-") else {
        return false;
    };
    let Some((id, suffix)) = rest.split_once('-') else {
        return false;
    };
    id.len() == LONG_RESOURCE_ID_HEX
        && is_lower_hex(id)
        && suffix.len() == VPCE_SUFFIX_LEN
        && suffix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Whether `url` is the regional endpoint of `service` in `region`
/// (`https://ec2.ca-central-1.amazonaws.com`) or the regional DNS name of an
/// interface VPC endpoint for it
/// (`https://vpce-0123456789abcdef0-abcd1234.ec2.ca-central-1.vpce.amazonaws.com`),
/// with at most one trailing slash. Anything else is refused: these are the
/// hosts a session token or a ServiceAccount token is sent to.
#[must_use]
pub fn is_service_endpoint(url: &str, service: &str, region: &str) -> bool {
    let url = url.strip_suffix('/').unwrap_or(url);
    if url == regional_endpoint(service, region) {
        return true;
    }
    let Some(host) = url.strip_prefix("https://") else {
        return false;
    };
    let tail = format!(".{service}.{region}.vpce.{}", dns_suffix_of_region(region));
    host.strip_suffix(&tail).is_some_and(is_vpce_label)
}

/// Whether `c` is one [`TAG_CHARACTERS_PATTERN`] accepts.
#[must_use]
pub fn is_tag_character(c: char) -> bool {
    c.is_ascii_alphanumeric() || " +=._:/@-".contains(c)
}

/// Checks a `Provider`'s extra tags (ADR-0172's settings, for EC2): at most
/// [`MAX_EXTRA_TAGS`]; keys 1 to 128 characters and values up to 256, of
/// [`TAG_CHARACTERS_PATTERN`]'s characters; no key starting `banlieue-` or
/// `aws:` in any case; no `Name`, which banlieue sets itself.
///
/// # Errors
/// A human-readable reason for the first rule broken.
pub fn validate_extra_tags(tags: &BTreeMap<String, String>) -> Result<(), String> {
    if tags.len() > MAX_EXTRA_TAGS {
        return Err(format!(
            "tags: at most {MAX_EXTRA_TAGS} extra tags fit beside banlieue's own (got {})",
            tags.len()
        ));
    }
    for (key, value) in tags {
        let lower = key.to_ascii_lowercase();
        if lower.starts_with(BANLIEUE_TAG_PREFIX) {
            return Err(format!(
                "tags: {key:?} starts with banlieue- (in any case), banlieue's own tags"
            ));
        }
        if lower.starts_with(AWS_TAG_PREFIX) {
            return Err(format!("tags: {key:?} starts with aws:, reserved by AWS"));
        }
        if key == NAME_TAG {
            return Err("tags: Name is set by banlieue to the machine's name".to_string());
        }
        if key.is_empty() || key.chars().count() > MAX_TAG_KEY_LEN {
            return Err(format!(
                "tags: a key is 1 to {MAX_TAG_KEY_LEN} characters (got {})",
                key.chars().count()
            ));
        }
        if value.chars().count() > MAX_TAG_VALUE_LEN {
            return Err(format!(
                "tags: the value of {key:?} is over {MAX_TAG_VALUE_LEN} characters"
            ));
        }
        if !key.chars().chain(value.chars()).all(is_tag_character) {
            return Err(format!(
                "tags: {key:?} uses a character outside ASCII letters, digits, space and + - = . _ : / @"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "aws_tests.rs"]
mod aws_tests;
