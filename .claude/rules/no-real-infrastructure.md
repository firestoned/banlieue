# Never Commit Real Infrastructure Identifiers

> **banlieue is a public OSS repository.** Anything committed here is published,
> indexed, and permanent — a later commit that removes it does not un-publish it.
> Real hostnames, addresses, and account identifiers from the maintainer's own
> environment MUST NOT appear in tracked files.

This is not a style preference. A real hostname in a public repo is a free
reconnaissance gift: it names a host, implies what runs on it, and often reveals
the naming scheme for every other host beside it.

## The rule

**Never write a real hostname, IP address, username, or account identifier
belonging to the maintainer's environment into any tracked file.** This applies
to code, tests, docs, ADRs, examples, scripts, Makefiles, comments, commit
messages, and changelog entries — everywhere, with no exceptions for "it's just
a doc comment" or "it's only an example in a test I marked `#[ignore]`."

If you need a concrete value to make an example readable, use a placeholder from
the table below.

## Placeholders to use

| Kind | Use | Never use |
| --- | --- | --- |
| Hostname / domain | `bar.foo.io`, `baz.foo.io`, `vcenter.example.com` | any real host the maintainer operates |
| Documentation IPv4 | `192.0.2.x`, `198.51.100.x`, `203.0.113.x` (RFC 5737) | any real routable address |
| Documentation IPv6 | `2001:db8::/32` (RFC 3849) | any real routable address |
| Private / cluster IPv4 | `10.0.0.x`, `192.168.x.x` (RFC 1918) — fine when the example is *semantically* a private network | a real private address that is actually in use |
| Public DNS resolver | `192.0.2.53`, `198.51.100.53` | `1.1.1.1`, `8.8.8.8` — real third-party services |
| Username | `admin`, `svc-banlieue` | a real login |
| Registry | `ghcr.io/firestoned/banlieue`, `registry.internal:5000` | a real private registry host |

**Prefer `foo.io`-style names** (`bar.foo.io`) for anything that stands in for a
maintainer-operated host. `example.com` / `example.org` (RFC 2606) are also fine
and are already used throughout `docs/`.

The RFC 5737 ranges are the right answer for "make up an IP" — they are reserved
for documentation and are guaranteed never routable. A *genuinely* random IP is
worse than a reserved one: it probably belongs to somebody.

## The one legitimate exception

`authors = ["Erick Bourgeois <erick@jeb.ca>"]` in `Cargo.toml` and
`docs/pyproject.toml` is package metadata the maintainer chose to publish. Leave
it alone. It is an author identity, not an infrastructure identifier — the
distinction is whether the string names *a host you could connect to*.

## Getting a real value in without committing it

Real environments still need testing. Take the value from the environment at
runtime and document it with a placeholder:

```rust
// ✅ GOOD — the real host comes from the environment, the doc shows a placeholder
//! LIBVIRT_HOST=bar.foo.io \
//!   cargo test -p banlieue-libvirt --test live_libvirtd -- --ignored
let host = std::env::var("LIBVIRT_HOST")
    .expect("set LIBVIRT_HOST, e.g. LIBVIRT_HOST=bar.foo.io");

// ❌ BAD — a real host is now in the git history, permanently
//! LIBVIRT_HOST=<the maintainer's actual hypervisor hostname> \
const DEFAULT_HOST: &str = "<the maintainer's actual hypervisor hostname>";
```

Shell scripts follow the same shape: default to empty and require the caller to
supply it, or derive it at runtime (`hostname -f`). Never bake one in as a
default — `scripts/bootstrap-k0s-cluster.sh` and the `K0S_*` Makefile variables
are the pattern to copy: every one of them defaults to empty.

## Before finishing any task

Grep your own diff. It costs one command:

```sh
# Real-infrastructure sweep over tracked files
git diff --cached -U0 | rg -i 'jeb\.ca|\b(?:\d{1,3}\.){3}\d{1,3}\b'
```

Flag anything that is not in the placeholder table above. If you are unsure
whether a value is real, assume it is and replace it.
