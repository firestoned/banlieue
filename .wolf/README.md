# `.wolf` — OpenWolf working directory

banlieue uses [OpenWolf](OPENWOLF.md) to manage context across development
sessions. Most of this directory is **local session state and is not checked
in**; three files are, because they hold knowledge a contributor needs and
would otherwise have to rediscover the hard way.

## What is tracked, and why

| File | Why it is public |
| --- | --- |
| [`OPENWOLF.md`](OPENWOLF.md) | The operating protocol — how sessions hand off, how memory is kept, how bugs are logged. Explains the rest of this directory. |
| [`cerebrum.md`](cerebrum.md) | **The most useful file here.** Project conventions, accumulated learnings, and a **Do-Not-Repeat** list: mistakes that were made, diagnosed, and must not recur. Each entry says what went wrong, why it was not obvious, and what to do instead. |
| [`buglog.json`](buglog.json) | Bugs with their root cause and fix, searchable with `openwolf bug search "<error>"`. Before debugging something, look here — it may already be solved. |
| [`anatomy.md`](anatomy.md) | A file and symbol index, queried by `openwolf find`. Its whole purpose is to answer "where is X" for a few hundred tokens instead of a grep across the tree, so it is exactly the sort of thing that belongs in the repo rather than in one person's working copy. |

## What is not tracked, and why

| Path | Why it stays local |
| --- | --- |
| `memory.md` | A chronological per-session action log. Large, noisy, rewritten every session, and specific to one machine's runs. |
| `STATUS.md` | Session handoff, true only until the next `/handoff`. |
| `hooks/` | **Compiled build artifacts of a separately-installed tool**, not banlieue source — there is no `.ts` or `.map` here, and `openwolf` is installed independently (Homebrew). Committing them would pin a stale snapshot of somebody else's build; install the tool and you get them. |
| `config.json`, `token-ledger.json`, `cron-*.json`, `cache/` | Local configuration, accounting and scratch. |

### A caveat on `anatomy.md`

The committed copy is the output of a **full rescan**:

```sh
openwolf scan
```

That rebuilds the index from the tree alone. A working copy's index also
accumulates entries for files a session merely *touched*, which can include
paths outside the repository — scratchpads, local plan directories. Those
leak absolute paths, so **run `openwolf scan` before committing any change to
this file**. The tracked version was verified to contain no out-of-repo
entries; an unscanned one had six.

## If you are reading this before making a change

Two habits, both cheap:

```sh
openwolf find <symbol-or-file>                   # where is it (cheap)
grep -A2 '## Do-Not-Repeat' .wolf/cerebrum.md    # what has already gone wrong
openwolf bug search "<your error message>"        # whether it is already fixed
```

The Do-Not-Repeat list is deliberately blunt about failures, including the
ones that looked like the obvious approach at the time. A few examples of the
kind of thing it records:

- a `kubectl port-forward` used to publish a service, which binds a *pod* and
  dies whenever that pod is replaced;
- a backup written into `/etc/kubernetes/manifests/`, where the kubelet
  parses every file and the backup won over the manifest it was backing up;
- an admission policy that validated a field against `request.userInfo` on
  UPDATE as well as CREATE, and so denied the controller that had to
  reconcile the object.

None of those were exotic. They were plausible first attempts whose failure
modes were quiet, which is exactly why they are written down.

## Editing these files

`cerebrum.md` and `buglog.json` are normally maintained by the OpenWolf
hooks, not by hand — see [`OPENWOLF.md`](OPENWOLF.md). If you add an entry
manually, keep the existing format so the tooling can still parse it, and
remember that this directory is **public**: no real hostnames, addresses,
usernames or credentials from your own environment
([`.claude/rules/no-real-infrastructure.md`](../.claude/rules/no-real-infrastructure.md)).

That rule has been broken here once already — a Do-Not-Repeat entry about not
publishing real hostnames contained one, and it was caught while preparing
this directory for publication.
