# EspoCLI

A command-line harness for EspoCRM, built for AI agents. Authenticate once, then read and
write records with output compact enough to hand straight to a model.

The project is EspoCLI; the command it installs is `espo`, kept short because an agent pays
for every character it types.

`espo schema Lead` is 3.2 KB. The `/api/v1/Metadata` blob it replaces is 820 KB.

## Why

Driving the EspoCRM REST API with `curl` costs three things: the URL and API key on every
call, a response shape nobody asked for (a `Lead` carries 105 attributes), and no way to
discover an entity's fields without downloading the whole metadata document.

EspoCLI fixes all three. stdout carries tab-separated data and nothing else, so a pipe or an
agent can parse it without filtering; counters and diagnostics go to stderr.

## What it saves

One task, measured three ways against a live instance: read the five most recent leads with
their last name, company and status. An agent that has never seen the instance must first
learn what those fields are called.

Learning the fields:

| | tokens | time |
|---|---|---|
| `curl GET /Metadata` | 202,775 | 482 ms |
| `curl GET /Metadata?key=entityDefs.Lead` | 4,362 | 252 ms |
| `espo schema Lead` | **855** | **17 ms** |

Running the query:

| | tokens | time |
|---|---|---|
| `curl GET /Lead?maxSize=5` | 4,321 | 2,355 ms |
| `curl` with `select=lastName,accountName,status` | 330 | 186 ms |
| `espo list Lead -s lastName,accountName,status -n 5` | **39** | 193 ms |

The whole task end to end: 207,096 tokens by the naive route, 4,692 with a hand-tuned
`curl` that already knows the schema, 894 with EspoCLI. The gap holds as rows grow, because
`select` is advisory in the API and returns eight attributes whatever you ask for, while
EspoCLI prints the columns you named:

| rows | curl tokens | EspoCLI tokens | ratio |
|---|---|---|---|
| 5 | 330 | 39 | 8.5x |
| 20 | 1,294 | 143 | 9.0x |
| 100 | 6,454 | 701 | 9.2x |
| 200 | 13,046 | 1,415 | 9.2x |

Three honest caveats. EspoCLI is **not faster per request**: 193 ms against 186 ms is the
same single round trip, and `--json` hands back the API's own bytes at the API's own token
cost. Comparing identical columns the saving is a flat 1.4x, not 9x; the 9x comes from
asking for three columns and getting three. And token counts are `cl100k_base`, which is
not Claude's tokenizer, so read the ratios rather than the absolute numbers.

The one real speed win is `schema`, served from a local cache. The one real slowdown avoided
is the naive call: asking for all 109 attributes of five leads takes 2.3 seconds, twelve
times a projected query, because the server computes every field to throw it away.

## Install

```sh
cargo install --path .
```

Requires Rust 1.85 or newer. The crate is `espocli` and the binary it installs is `espo`.

## Authenticate

Create an API user in EspoCRM under Administration > API Users, then:

```sh
espo auth login --url https://crm.example.com --api-key YOUR_KEY
espo status
```

`espo status` is the one call to make when something looks wrong. It reports the active
profile, the instance behind it, the EspoCRM version, who you are authenticated as and how
old the metadata cache is. `espo auth status` prints the same thing.

```
profile   prod
url       https://crm.example.com
instance  Example CRM
version   10.0.5
user      integrations
type      api
entities  75
cache     4m ago
```

It makes a single API call and never downloads metadata just to fill in a count: an empty
cache reports `empty`, and `espo entities` warms it.

The key is stored in `~/.config/espo/config.toml` with mode 600. Pipe it in to keep it out
of your shell history:

```sh
echo "$KEY" | espo auth login --url https://crm.example.com
```

Several instances are handled with named profiles:

```sh
espo auth login --profile staging --url https://staging.example.com --api-key KEY
espo list Lead --profile staging
```

`ESPO_PROFILE` selects a profile. `ESPO_URL` plus `ESPO_API_KEY` override the file
entirely, which is what you want in CI and containers.

## Discover the instance

```sh
espo entities                    # every entity type, with its module
espo entities -o                 # only the ones users see as records
espo schema Lead                 # field  type  required  options
espo schema Lead -f status       # one field, with its enum values
espo schema Lead --links         # link  type  target entity
```

Metadata is cached for 24 hours under `~/.cache/espo/<profile>/`. `--refresh` refetches it.
Entity names are matched case-insensitively, so `espo list lead` works.

## Read

```sh
espo list Lead -w status=New -n 10
espo list Lead -w 'status:New,Assigned' -w 'assignedUserId=null' --order createdAt --desc
espo list Lead -w 'name~garcia' -s id,name,emailAddress
espo get Lead 00000000000000001
espo related Account 5f3a1b2c defaultContact
espo report 00000000000000002 -n 50
```

`report` runs an Advanced Pack report of type List and prints its own columns, with `id`
prepended so a row can be looked up with `get`. A column reaching into a related record,
which the report names `account.industry`, is resolved to the value the row carries.
Grid reports are not supported; `raw` reaches them.

Filters are short expressions. `-w` repeats and the terms are ANDed:

| Expression | Meaning |
|---|---|
| `field=value` | equals |
| `field!=value` | not equals |
| `field>v` `field>=v` `field<v` `field<=v` | comparisons |
| `field~text` | contains |
| `field^text` | starts with |
| `field:a,b,c` | in |
| `field!:a,b,c` | not in |
| `field=null` / `field!=null` | is null / is not null |

For OR, nesting or an Espo filter type not covered above, pass the clause through:

```sh
espo list Lead --where-json '[{"type":"or","value":[
  {"type":"equals","attribute":"status","value":"New"},
  {"type":"equals","attribute":"source","value":"Web Site"}]}]'
```

Without `-s`, `list` selects `id`, `name` and whatever you filtered or sorted on. That is
the difference between a 3-column row and 105 attributes. `-s '*'` returns everything.
`--no-total` skips the count, which is noticeably faster on large entities.

## Write

```sh
espo create Lead lastName=Garcia firstName=Ana status=New
espo update Lead 00000000000000001 status=Dead
espo delete Lead 00000000000000001 --yes
espo link Lead 00000000000000001 teams TEAM_ID
espo unlink Lead 00000000000000001 teams TEAM_ID
```

Values are converted to the field's declared type, read from the metadata cache: a `bool`
field gets a real boolean, an `int` gets a number, a `multiEnum` gets a list split on
commas. A phone number keeps its leading zeros because `phone` is a string field. Force a
raw value with `:=`:

```sh
espo create Lead lastName=Garcia 'teamsIds:=["t1","t2"]'
```

Longer bodies come from a file or a pipe, and inline pairs override them:

```sh
cat lead.json | espo create Lead --data - status=New
```

`delete` refuses to run without `--yes`. **Check any write with `--dry-run` first**: it
prints the exact request and sends nothing.

```sh
espo --dry-run update Lead 00000000000000001 status=Dead
# PUT https://crm.example.com/api/v1/Lead/00000000000000001
# {"status":"Dead"}
```

## Anything else

`raw` reaches every endpoint that has no dedicated command: streams, attachments, actions,
admin.

```sh
espo raw GET Note -q 'searchParams={"maxSize":5}'
espo raw POST Lead/00000000000000001/convert --data '{"entityType":"Contact"}'
espo raw POST Contact -H 'X-Skip-Duplicate-Check: true' --data '{"lastName":"Garcia"}'
```

It prints the response as compact JSON. `-H` repeats, and is the way to reach the API's
header-only switches such as `X-Skip-Duplicate-Check`, which turns the 409 on a duplicate
record into a normal create.

## Output

TSV by default, with a header row of real attribute names. `--no-header` drops it,
`--json` prints the raw API response instead.

Tabs inside a value become spaces and newlines become a literal `\n`, so a row is always
one line. Use `--json` when you need the value back byte for byte.

## Security

- **The API key is stored in `~/.config/espo/config.toml`, created mode 600 in a mode 700
  directory.** The file is opened with those permissions rather than chmod'd afterwards, so
  the secret is never briefly world-readable.
- **Pipe the key in rather than passing `--api-key`.** A command-line argument is visible to
  any process on the machine through `ps`, and it lands in your shell history.
- **Plaintext `http://` is refused** for anything but `localhost`, because the key travels in
  a request header. `https://` only.
- **Record values are treated as untrusted.** A CRM field containing ANSI escapes, control
  characters or bidi overrides cannot drive your terminal or forge a column: they are
  stripped from TSV output. `--json` returns the raw bytes, so handle that output as data.
- **The metadata cache is keyed by profile and instance URL** and written mode 600. Two
  instances never share a cached schema, so a repointed profile cannot serve the wrong
  field types.
- Profile names are restricted to letters, digits, `-`, `_` and `.` because they become path
  components.
- TLS uses rustls with the platform's root store. Certificate validation is never disabled;
  there is no flag to turn it off.

What the CLI does *not* protect you from: the permissions of the API user itself. Give it
the narrowest role that does the job. `espo` can only do what that user can do.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | API or network error |
| 2 | usage error |
| 3 | authentication failure (401, 403) |
| 4 | not found (404) |

Failures print one line to stderr, taken from Espo's `X-Status-Reason` header:

```
error: 404 GET Lead/nope: Record nope does not exist.
```

## For agents

- Parse stdout only. It is data. Everything else is on stderr.
- Run `espo schema <Entity>` before writing to one. It is cheap and it tells you the field
  names, the required fields and the allowed enum values.
- Always `--dry-run` a write you are unsure about.
- `delete` needs `--yes`, and needs an API user with delete permission.

## Tests

```sh
cargo test                       # unit tests, no network
ESPO_TEST_URL=https://crm.example.com ESPO_TEST_API_KEY=KEY cargo test -- --ignored
```

The live tests are read-only and dry-run only. They create nothing.

## How it works

`docs/how-it-works.md` covers the internals: the request flow, why the metadata cache exists
and how it is keyed, how default columns are chosen, how the filter DSL is parsed, and where
to add a command. Read it before changing the code.

## Not here yet

Attachment upload helpers, stream helpers, mass update and delete, user-defined aliases,
aligned table output, shell completion, HMAC authentication. `raw` reaches those endpoints
in the meantime.
