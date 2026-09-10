# How espo works

The program is built on one idea: the EspoCRM instance already knows its own schema, so the
CLI should read that schema once, cache it, and use it to decide what to send and what to
print. Everything else follows from that.

Without the schema, a generic CRM client has to either return every attribute a record has
or make the caller name each one. With it, `espo` picks three columns instead of 105, sends
a boolean as a boolean, and rejects a misspelled entity name before touching the network.

## What happens when you run a command

`run()` in `src/main.rs` does the same five things for every command except `auth`:

1. Parse the arguments with clap.
2. Load `~/.config/espo/config.toml` and resolve which profile applies.
3. Build a `Client` from that profile's URL and API key.
4. Load the metadata, from the local cache when it is fresh.
5. Dispatch to the branch for the command.

`auth` returns before step 2 because it is the command that writes the config rather than
reading it.

Take `espo list Lead -w status=New -n 3`. After the client exists, the list branch resolves
`Lead` against the cached entity names, asks `select_columns` which attributes to request,
converts `status=New` into an Espo where clause, packs both into a `searchParams` JSON
object, sends one GET, and prints the rows projected to the chosen columns. One network call,
three columns, a count on stderr.

## The metadata cache

`GET /api/v1/Metadata` returns most of a megabyte on a mid-sized instance: every entity
definition, the
type of every field, the allowed values of every enum, and the target of every link. Fetching
that on each invocation would make the tool slower than curl.

So `Meta::load` writes it to `~/.cache/espo/<profile>-<hash>/metadata.json` and reuses it for
24 hours. A warm run is an order of magnitude faster than a cold one.

The `<hash>` is eight hex digits derived from the instance URL, and it exists because of a
bug worth knowing about. When credentials come from `ESPO_URL` and `ESPO_API_KEY` there is no
profile name, so the resolver calls the profile `env`. Two different instances driven from
environment variables therefore shared one cache entry, and the second one got the first
one's field types. Including the URL in the directory name fixes that, and it also means
repointing a named profile at a different server invalidates its cache instead of serving a
stale schema.

`--refresh` refetches. Nothing else expires the cache early.

## Which columns get requested

This is where most of the token saving comes from. When `--select` is absent,
`select_columns` builds the list itself:

- `id`, always.
- `name`, if the entity has that field.
- Every field named in a `--where` expression.
- The `--order` field.

So `espo list Lead -w 'status:New,Assigned' --order createdAt` asks for `id`, `name`,
`status` and `createdAt`. Ten leads come back as 411 bytes. The same ten with every attribute
are 29 KB, which is 70 times more for information nobody asked for.

Espo does not honor `select` exactly. It adds `createdAt`, `createdById` and `assignedUserId`
to every list response whatever you request, because its ACL layer needs them. `emit_list`
therefore projects the rows to the requested columns in the requested order rather than
printing whatever arrived. `--select '*'` sends no `select` parameter and prints the keys of
the first row, sorted.

## How filters are parsed

`query.rs` walks each `--where` string looking for the first position where an operator
matches, trying two-character tokens before one-character ones so that `!=` wins over `=`.
The text before the operator is the attribute, the text after is the value, and a table maps
the operator to an Espo where type.

Three cases are special. `field=null` and `field!=null` become `isNull` and `isNotNull`, which
take no value. `field:a,b` and `field!:a,b` split the value on commas and produce an array.

Repeated `--where` flags append to the same clause list, and Espo ANDs a flat list. There is
no syntax for OR because expressing a clause tree in a flag string turns into a parser nobody
wants to debug. `--where-json` takes the array or object verbatim instead, and it merges with
whatever the DSL produced.

## Why values are typed from metadata

A CRM cares about the difference between `"true"` and `true`. Guessing from the text alone
gets it wrong in both directions: a numeric-looking string like the phone number
`0034931234567` loses its leading zeros, while `isActive=true` arrives as a string that a
boolean field may or may not accept.

`coerce` looks up the field's declared type and converts accordingly. A `bool` field gets a
boolean, `int` gets an integer, `float` and the two currency types get a number, and `array`,
`multiEnum`, `checklist` and `linkMultiple` get a list split on commas. Anything else stays a
string, which is also what happens when the field is unknown. That last case is deliberate:
`assignedUserId` is not a field in the metadata, its link `assignedUser` is, and treating the
id as text is correct.

One exception is hard-coded. An attribute ending in `Ids` whose base name is a `linkMultiple`
field becomes a list, so `teamsIds=t1,t2` produces `["t1","t2"]`. Espo expects that shape and
no metadata entry describes it.

`field:=<json>` skips coercion entirely and parses the value as JSON.

## Output

stdout carries tab-separated data and nothing else. Counts, confirmations and error messages
go to stderr. A script or an agent can read stdout without filtering, and a person still sees
`3/1204` after a query.

Record values come from a CRM, which means a stranger filling in a web form can influence
them. `escape` in `output.rs` replaces tabs with spaces, turns newlines into a literal `\n`,
and drops every control character, DEL, C1 byte and bidi override. A field containing
`\x1b[2J` cannot clear the reader's terminal, and no value can end a row early and forge a
column. `--json` prints the API response unmodified, so treat that output as data rather than
as something to paste into a terminal.

## Errors

Espo puts the reason for a failure in the `X-Status-Reason` header and usually returns an
empty body. `Client::send` reads that header, falls back to the body and then to the HTTP
status text, and wraps the result in an `ApiError` carrying an exit code.

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | API or network error |
| 2 | Usage error |
| 3 | Authentication failure, 401 and 403 |
| 4 | Not found, 404 |

`main` downcasts the error to recover the code, so a failed `get` exits 4 and a bad filter
exits 2 without either one needing to know about the other.

Usage errors are detected locally where possible. An unknown entity name, a link that the
entity does not have, and an unparseable filter all fail before any request goes out, and
each message names the command that would answer the question: `run: espo entities`,
`run: espo schema Lead --links`.

## Dry run

`--dry-run` prints the method, the URL and the body, then sends nothing.

The mechanic is a split in `client.rs`. `request` checks the flag and either prints or
delegates; `send` always performs the call. Metadata loading uses `send`, because the schema
is what the CLI needs in order to build the request that `--dry-run` describes. An earlier
version routed everything through `request`, which meant a dry run got an empty metadata
document and failed with `metadata has no entityDefs`. The integration test caught it.

## Where the code lives

| File | Lines | Responsibility |
|---|---|---|
| `main.rs` | 703 | Command definitions, dispatch, rendering per command |
| `query.rs` | 225 | Filter DSL, value coercion, `field=value` parsing |
| `config.rs` | 167 | Profiles, TOML read and write, restricted file creation |
| `meta.rs` | 156 | Metadata fetch, cache, queries over the schema |
| `client.rs` | 140 | HTTP, auth header, error mapping, dry run |
| `output.rs` | 129 | TSV rendering, escaping, column selection |

`main.rs` is the one to watch. It holds the clap definitions, the dispatch table and the
per-command rendering, and at 703 lines it is already twice the size of anything else. The
next command added to it is a good moment to move the rendering helpers into `output.rs` and
the command bodies into a `commands` module. It has not crossed that line yet, so it stays as
it is.

## Extending it

Two paths, and the second is usually right.

For a one-off call, use `raw`. It takes a method, a path under `/api/v1`, an optional JSON
body and repeatable query parameters, and it prints the response as compact JSON. Streams,
attachments, record actions and admin endpoints are all reachable this way with no code.

Add a real command when a call needs the schema: default columns, typed values, a validated
entity or link name, or TSV output. That means a variant in the `Command` enum, a branch in
the `match`, and a rendering call. The existing branches are short because the shared work
lives in `select_columns`, `search_params` and `emit_list`.

## Tests

`cargo test` runs 12 unit tests and touches no network. They cover the three pieces with real
logic: the filter parser, value coercion and output escaping. The escaping tests assert that
ANSI sequences, DEL, C1 bytes and bidi overrides do not survive, and the config tests assert
that a profile named `../../pwned` is rejected before it can place a cache directory outside
its parent.

The two tests in `tests/live.rs` run against a real instance and are ignored unless
`ESPO_TEST_URL` and `ESPO_TEST_API_KEY` are set. They read and they dry-run; they create nothing,
so they are safe to point at an instance you care about.
