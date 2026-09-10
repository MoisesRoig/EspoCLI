# EspoCLI design

**Date:** 2026-09-01
**Status:** approved, implementing

## Problem

Driving the EspoCRM REST API from a shell today means `curl`. That costs three things:

1. **Repeated authentication.** Every call carries the base URL and the API key header.
2. **Tokens.** `GET /api/v1/Metadata` is 825 KB. `GET /api/v1/App/user` is 38 KB. A
   `GET /api/v1/Lead` without `select` returns 105 attributes per record. For an AI agent
   every call burns a large slice of the context window.
3. **Discovery.** Learning an entity's fields, an enum's allowed values or a link's name
   requires reading the whole Metadata blob.

No CLI exists for the EspoCRM remote API. `espo-console` and `command.sh` are server-side
PHP administration tools (rebuild, backup, upgrade), a different problem.

## Goal

An `espo` binary in Rust covering the full CRUD surface of the EspoCRM API, authenticating
once, designed so the primary consumer is an AI agent on a token budget.

Reference instance: a mid-sized EspoCRM 10.x deployment, roughly 80 entityDefs and
half of them record entities.

## Decisions

| Decision | Choice |
|---|---|
| Default output | Compact TSV. `--json` opt-in. |
| Credentials | TOML file, mode 600, under `~/.config/espo/`. |
| Writes | `create`/`update` unrestricted. `delete` requires `--yes`. Global `--dry-run`. |
| v1 scope | Generic core plus introspection. |
| Architecture | Fixed commands, entity as an argument, driven by cached metadata. |

Rejected: generating commands at runtime from metadata (dynamic clap is unmanageable, the
`--help` becomes useless, breaks on a stale cache) and user aliases in the style of
jira-cli (30 lines on top of this design, added once real repetition shows up).

## Architecture

No async. `reqwest::blocking` over rustls. Dependencies: `clap`, `reqwest`, `serde`,
`serde_json`, `toml`, `anyhow`. Nothing else: no table library, no color library, no
`dirs` crate (XDG paths are three lines of stdlib).

| File | Responsibility |
|---|---|
| `main.rs` | CLI definition and dispatch. |
| `config.rs` | Profiles, reading and writing `config.toml`. |
| `client.rs` | HTTP, auth headers, error mapping, `--dry-run`. |
| `meta.rs` | Metadata fetch, cache and compact queries over it. |
| `query.rs` | Filter mini-DSL to Espo `searchParams`, value coercion. |
| `output.rs` | TSV and JSON rendering, escaping. |

## 1. Authentication and profiles

```
espo auth login --url <url> --api-key <key> [--profile <name>]
espo auth status
espo auth logout [--profile <name>]
```

`login` verifies the credential with `GET App/user` before storing it. Config lives at
`$XDG_CONFIG_HOME/espo/config.toml` (falling back to `~/.config`), mode 600:

```toml
default = "prod"

[profiles.prod]
url = "https://crm.example.com"
api_key = "..."
```

Profile resolution: `--profile` > `ESPO_PROFILE` > `default`. `ESPO_URL` plus
`ESPO_API_KEY` override the file entirely, for CI and containers.

API key only in v1. HMAC and username/password are out: for an agent the API key is the
right credential, it does not expire and it does not cross 2FA.

## 1b. Status

```
espo status
```

A single `GET App/user` call plus the local cache, rendered as `key<TAB>value`: active
profile, instance URL, application name, EspoCRM version, authenticated user and type,
entity count and metadata cache age. `espo auth status` shares the implementation.

Status never fetches metadata to fill in the entity count; an absent cache reports `empty`.
A status command that downloads 820 KB is not a status command.

Instance permissions are deliberately not reported. The `acl` block in `App/user` only
enumerates a subset of scopes and leaves the rest null, so any permission summary derived
from it would be misleading.

## 2. Introspection

```
espo entities                  # record entities, one per line
espo schema <Entity>           # field  type  required  options
espo schema <Entity> -f <field>
espo schema <Entity> --links   # link  type  target entity
```

Cached at `$XDG_CACHE_HOME/espo/<profile>/metadata.json`, 24 h TTL, `--refresh` forces a
reload.

`espo schema Lead` is roughly 1,000 tokens against 825 KB of raw Metadata. This is the
main reason the tool exists.

## 3. Filters: a mini-DSL

`--where` takes short expressions. Repeated, the terms are ANDed.

| DSL | Espo type |
|---|---|
| `field=value` | `equals` |
| `field!=value` | `notEquals` |
| `field>v` `field>=v` `field<v` `field<=v` | `greaterThan`, `greaterThanOrEquals`, `lessThan`, `lessThanOrEquals` |
| `field~text` | `contains` |
| `field^text` | `startsWith` |
| `field:a,b,c` | `in` |
| `field!:a,b,c` | `notIn` |
| `field=null` | `isNull` |
| `field!=null` | `isNotNull` |

For OR, nesting or uncovered types, the escape hatch is `--where-json '<json>'`, passed
through untouched.

`--where 'status=New'` is 5 tokens. The equivalent JSON is 25.

## 4. Output

**stdout carries data only.** The result counter and every diagnostic go to stderr so an
agent can parse stdout without filtering stray lines.

**Default field selection:** `id`, plus `name` when the entity has it, plus every field
appearing in `--where` or `--order`. Without this Espo returns all 105 attributes and the
saving is lost. `--select '*'` lifts the restriction.

Espo always adds `createdAt`, `createdById` and `assignedUserId` to a list response
regardless of `select`, so columns are projected client-side to the requested set and
order.

**TSV escaping:** tab becomes a space, newline becomes a literal `\n`. Lossy and
deterministic. Callers needing fidelity use `--json`.

Header row uses the real attribute names, suppressed with `--no-header`.

## 5. CRUD, relationships and escape hatch

```
espo list <Entity> [-w ...] [-n N] [--offset N] [--order field] [--desc] [-s ...]
espo get <Entity> <id> [-s ...]
espo create <Entity> field=value ... [--data <json>]
espo update <Entity> <id> field=value ... [--data <json>]
espo delete <Entity> <id> --yes
espo link <Entity> <id> <link> <targetId>...
espo unlink <Entity> <id> <link> <targetId>...
espo related <Entity> <id> <link> [-w ...] [-n N]
espo raw <METHOD> <path> [--data <json>] [-q k=v]...
```

`raw` is what makes the tool cover the whole API rather than the subset with dedicated
commands: streams, attachments, actions, admin endpoints. One command instead of forty.

**Metadata-driven value coercion.** `field=value` is converted to the field's declared
type: `bool` to a boolean, `int` and `float` to numbers, `array`/`multiEnum`/`checklist`
to a list split on commas, everything else to a string. An unknown field stays a string.
A `<link>Ids` attribute whose link is `linkMultiple` becomes a list. `field:=<json>`
forces raw JSON.

This keeps a phone number `0034931234567` from becoming a number and keeps `isActive=true`
from arriving as a string. It is the second reason the metadata cache pays for itself.

`--data <json>` accepts a full JSON object, or `-` to read it from stdin. It merges with
the `field=value` pairs, which win on conflict.

## 6. Errors and exit codes

EspoCRM returns the reason in the `X-Status-Reason` header with an empty body. It is
emitted to stderr as one short line.

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | API or network error |
| 2 | Usage error |
| 3 | Authentication failure (401, 403) |
| 4 | Not found (404) |

## 6b. Security posture

The credential is the asset. Decisions, and what each one is defending against:

| Control | Threat |
|---|---|
| Config file opened `O_CREAT` with mode 600 inside a 700 directory | A chmod after the write leaves the key world-readable for a moment. |
| API key accepted on stdin, documented in preference to `--api-key` | Arguments are visible via `ps` and persist in shell history. |
| `http://` refused except for loopback | The key is a request header; plaintext exposes it on the wire. |
| Control characters, DEL, C1 and bidi overrides stripped from TSV values | A CRM field is attacker-controllable in a lead-capture form. An ESC introducer lets it drive the reader's terminal or forge a column. |
| Metadata cache keyed by profile *and* URL hash, mode 600 | Env credentials always resolve to the profile name `env`, so two instances would otherwise share one cached schema and coerce values against the wrong field types. |
| Profile names restricted to `[A-Za-z0-9._-]{1,64}` | The name becomes a path component; `../../x` escaped the cache directory. |
| rustls with the platform root store, no opt-out | No accidental downgrade to unvalidated TLS. |

Instance permissions are the API user's role, not the CLI's job. The tool cannot exceed
what that user is allowed to do, and it does not try to summarise those permissions
(see 1b).

## 7. Verification

Unit tests next to the code for the three pieces with real logic: the DSL parser, value
coercion and TSV escaping. One integration test against a live instance behind
`ESPO_TEST_URL`, ignored by default.

## Out of scope for v1

Attachment upload and download helpers, stream helpers, mass update and delete, user
aliases, aligned `--table` output, shell completion, HMAC authentication. Added on request.
The `raw` command reaches all of these endpoints in the meantime.
