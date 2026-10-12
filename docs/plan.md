# wattdb implementation plan (approved 2026-10-12)

This turns [decisions.md](decisions.md) (D1–D58) into a build order. The milestone order, porting
order, CI and Go version were approved on 2026-10-12. The package layout below follows the
decisions, and anything new it raises gets brought to the owner before it's built.

The behavior to reproduce is specified in [design.md](design.md) (the ElectroDB 3.9.3 compatibility contract).

---

## 1. Repository layout

```
wattdb/                          module github.com/outboundani/wattdb
  wattdb.go, config.go           NewConfig, options, Cursor, Page[T], Outcome[T]        (D25, D31, D32, D37)
  errors.go                      ErrNotFound, ErrConditionFailed, ... + typed errors   (D28, D48)
  expr.go                        And/Or/Not, Raw/Name/Value                            (D36)
  tx.go                          TxWrite / TxGet over op values                        (D42)
  hooks.go, slog.go              middleware hooks + slog adapter                       (D50)
  attr/  index/  codec/          schema DSL: attribute + index builders, codecs        (D13, D14, D18)
  internal/model/                normalized model (equivalent of ElectroDB _parseModel)
  internal/engine/               the ElectroDB engine (D23):
    keys.go                        prefixes, labels, casing, padding, templates, cast, clustered, scope
    item.go                        default/set/watch/validate on write; get/hidden on read; field mapping
    update.go                      update/patch/upsert expressions, index maintenance, sparse conditions
    query.go                       key conditions, sort-key ops, compare modes, ownership filters
    cursor.go                      ElectroDB-compatible cursors
    batch.go  tx.go  find.go  conv.go  parse.go
  internal/gen/                  generator: phase 1 (AST → entity types), phase 2 (load schema → rest) (D10, D11)
  cmd/wattdb/                    `wattdb generate`, `wattdb check`                     (D22)
  testdata/vectors/              recorded ElectroDB golden vectors, generic fixtures   (D8)
  private/                       gitignored: proprietary models, notes, vectors
  tools/recordvectors/           Node script, pinned electrodb@3.9.3, re-run on pin bumps
  examples/                      runnable examples (original spec)
  docs/                          decisions.md, design.md, plan.md, compat.md
```

Generated output in a user's module (D22, D24, D43, D11, D47):

```
db/<entity>/        client, struct, Input, Partial, Key, Filter, descriptors, enums, nested types
db/<entity>v2/      later schema versions + MigrateFrom
db/<service>/       collection clients + typed collection results
db/runtime/         callback registration (blank-imported by apps)
```

---

## 2. Milestones

Each milestone ends with tests passing and nothing half-built.

### M0: Skeleton and vector recorder
- Module layout, CI (`go test`, `go vet`, gofmt check).
- `tools/recordvectors`: a Node script (pnpm, exact pins) that loads ElectroDB 3.9.3, runs JSON
  scenario definitions with named callbacks through `.params()` and `.go()` against a fake transport,
  and writes wire-form vectors to `testdata/vectors/*.json` (D52–D55; format in its README).
- **Seed scenarios:**
  - The probes already run in this project (isolated/clustered keys, sub-collections, padding,
    numbers, no-sk, updates).
  - Generic fixture entities covering the feature list in section 3.
  - **Private vectors** (not committed): the same recorder can be pointed at a private model set,
    writing to the gitignored `private/vectors/`. Tests load those when the directory exists and
    skip otherwise.

### M1: Model and key engine (no DynamoDB)
- `internal/model`, built from a Go-literal model (the DSL comes later).
- `internal/engine/keys.go` covers all of design.md §1.1–1.3 (numbers per D56), including:
  - attribute-as-key-field rewriting
  - numeric cast keys
  - the lowercasing of stored attribute values when an attribute is also the key field
- `internal/vectors`: loads vectors, builds the model from their embedded JSON, decodes wire
  AttributeValue JSON, and holds the Go side of the `$fn` registry.
- **Tests:** every key in the vectors matches byte for byte, except the D56 number cases.

### M2: Item pipeline
- Attribute types: string/number/boolean/enum/map/list/set/any, plus codecs.
- Write order: default → set (watchers last) → validate.
- Read order: get (watchers last) → hidden.
- Field translation (including nested maps), identifiers, ownership checks, empty map/set rules.
- **Tests:** `put` items and read formatting match the vectors.

### M3: Schema DSL and generator
- `attr`, `index` and `codec` packages; typed `wattdb.Service`.
- Generator phases 1 and 2, the `runtime` package, and `wattdb check` validations (unknown
  attributes, sort-key types that can't be sorted, index/field collisions, collection mismatches,
  template/composite mismatches).
- Generated output is gofmt'd, deterministic, and carries the `// Code generated by wattdb. DO NOT EDIT.`
  header.
- **Tests:** golden files for the generic fixture entities, checked against the model from M1.
  Private models get the same checks when present (gitignored).

### M4: Single-item operations
- Get, Put, Create, Delete, Remove, Update, Patch, Upsert.
- Where/conditions, Raw, `.Params()`, response modes and Partial, `.ReturnRejected()`, overrides.
- **Integration tests:** DynamoDB Local via testcontainers-go. Items written by Go must equal the
  recorded ElectroDB items.

### M5: Reads at scale
- Query (per-composite sort-key steps), Scan (including index scans), Pages/Count/Limit/Order/Consistent.
- Cursors, `All` and `Pages` iterators.
- Projection, `.Raw()`, `.IncludeKeys()`, `.Hydrate()`, compare modes.

### M6: Multi-item and advanced features
- Batch get/put/delete (chunking, concurrency, preserve order, opt-in retry).
- TxWrite/TxGet.
- Collections (service packages, isolated/clustered/composite, sub-collections).
- Sparse index conditions, multi-attribute (composite) GSIs.
- Find/Match, conversions, Parse.

### M7: Operations and docs
- Hooks and slog adapter, versioned packages with `MigrateFrom`.
- `examples/`, `docs/compat.md` (every place we deliberately diverge, per D3), README.

---

## 3. Feature coverage order for fixtures

Generic fixture entities (users and tasks in a `taskapp` service, in the style of ElectroDB's public
docs) are added in this order, fewest features first:

| # | Features exercised |
|---|---|
| 1 | attribute-as-key string pk, enums, numbers, create/get/delete |
| 2 | GSI on an attribute field, query |
| 3 | numeric GSI key (`cast: number`), stored-value lowercasing |
| 4 | numeric primary key, `any` attribute |
| 5 | templates with `casing: none`, set `add`, `response: updated_new`, function defaults |
| 6 | template key with lowercasing, typed `any` (`map[string]string`) |
| 7 | attribute-as-key pk+sk (numeric sk), list of maps, `default: true` on a required bool |
| 8 | standard prefixed keys, multiple GSIs, enums with defaults, nested map/list, scan with all pages |
| 9 | deep nested maps, `any`, heavy update usage |

---

## 4. Approved (2026-10-12)

- Build milestone by milestone, M0 through M7.
- Fixtures and ports follow the feature order in section 3. The concrete porting order for the
  maintainers' own models is private (`private/porting-order.md`, gitignored).
- CI runs on GitHub Actions with a Go 1.27 floor: build, vet, gofmt and unit tests on every push,
  plus integration tests against DynamoDB Local (testcontainers) on Linux runners.
- Node is needed only by whoever re-records the vectors (D8).
