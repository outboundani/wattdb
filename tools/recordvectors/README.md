# recordvectors

Records golden vectors from ElectroDB for wattdb's Go tests (decisions D8, D52–D55). Go tests
read the committed vectors in `testdata/vectors`, so CI never needs Node. Run this tool only when a
scenario changes or the ElectroDB pin moves.

```sh
cd tools/recordvectors
pnpm install
pnpm record                  # every scenario in scenarios/ → ../../testdata/vectors
pnpm record numbers probes   # only the named scenarios
```

Owner-only models stay out of git. Put their scenarios in the gitignored `private/` directory and
record them there:

```sh
pnpm record --scenarios ../../private/scenarios --out ../../private/vectors
```

Pins are exact: `electrodb@3.9.3` and AWS SDK `3.654.0` (ElectroDB's own minimum), with
`pnpm-lock.yaml` committed. Recording is deterministic. Running it twice gives identical files.

## How it works

ElectroDB gets a real v3 `DynamoDBDocumentClient` with default marshall options. Only the HTTP
layer is fake (`transport.mjs`), so every request goes through lib-dynamodb marshalling and the AWS
JSON protocol exactly as in production. The transport:

- logs each request body in wire form;
- answers with the case's scripted responses in order, falling back to an empty success
  response for that operation;
- turns a body containing `__type` into an HTTP 400, which the SDK raises as that AWS error.

## Scenario format

```jsonc
{
  "description": "...",
  "table": "table",                 // optional, defaults to "table"
  "entities": {
    "user": {
      "model": {...}, "attributes": {...}, "indexes": {...},   // an ElectroDB v1 schema
      "config": {...}               // optional extra Entity config
    }
  },
  "services": {                     // optional
    "taskapp": { "entities": ["user", "task"], "config": {...} }
  },
  "cases": [
    {
      "name": "query by role",
      "entity": "user",             // or "service": "taskapp"
      "chain": [["query.byRole", { "orgId": "O-1" }], ["gte", { "userId": "U" }]],
      "params": {},                 // .params() options, or false to skip
      "go": {},                     // .go() options, or false to skip
      "responses": [{ "Items": [], "Count": 0 }]  // scripted wire responses for .go()
    }
  ]
}
```

`chain` is a list of `[method, ...args]` calls starting from the entity or service. A dotted method
such as `query.byRole` walks properties first. A step with no arguments may name a property instead
of a method, as in `["scan"]`. Each entity also has `conversions` (ElectroDB's `createConversions(entity)`), e.g.
`["conversions.fromComposite.toKeys", {...}]`. If the chain doesn't end in something with `.params()`/`.go()` (for
example `parse` or `conversions.*`), its return value is recorded as `recorded.value`.

### Named callbacks

JSON can't hold functions, so a callback or JS-only value is written as
`{"$fn": "<name>", "args": [...]}`. Anywhere in a schema, chain or options, it resolves to
`registry[name](...args)` from `registry.mjs`. The Go test harness keeps a registry with the same
names and semantics, so a new entry has to be added on both sides.

| Name | Resolves to |
|---|---|
| `undefined` | `undefined` |
| `regexp(source, flags)` | a `RegExp` (e.g. for `validate`) |
| `jsSet(values)` | a JS `Set` |
| `const(value)` | `() => value` (for `default`) |
| `upper`, `lower` | get/set transforms on strings |
| `prefix(p)` | get/set: `p + value` (undefined passes through) |
| `join(sep, ...attrs)` | set (with `watch`): sibling values joined by `sep` |
| `minLength(n)` | validate: undefined, or `String(value).length >= n` |
| `throws(message)` | validate that throws |
| `has(attr)` | index condition: `attr` is defined |
| `equals(attr, value)` | index condition: `attr === value` |
| `where(expr)` | where/condition callback built from an expression tree |
| `data(...statements)` | update `data()` callback |
| `tx(...ops)` | transaction callback; each op is `{"entity", "chain", "commit"}` |

A `where` expression is `["and", e...]`, `["or", e...]`, `["not", e]`, or
`[op, "attr.path", ...values]` for any ElectroDB where operation (`eq`, `between`, `begins`,
`exists`, ...). A `data` statement is `[op, "attr.path", ...values]`, e.g. `["set", "a.b[0]", 1]`.

## Vector format

A vector is its scenario, unchanged, plus the ElectroDB version and a `recorded` object on each
case:

```jsonc
{
  "electrodb": "3.9.3",
  ...scenario,
  "cases": [
    {
      ...case,
      "recorded": {
        "params": [{ "target": "PutItem", "request": {...} }],   // or { "error": {...} }
        "go": {
          "result": {...},      // what .go() resolved to, or
          "error": {...},       // what it threw
          "requests": [{ "target": "Query", "request": {...} }]
        },
        "value": {...},         // for chains that return a plain value
        "error": {...}          // the chain itself threw while being built
      }
    }
  ]
}
```

- `request` is the exact JSON body sent to DynamoDB, in wire `AttributeValue` form.
- `result` and `value` are JS values converted to `AttributeValue` JSON, so numbers keep their JS
  string form (`{"N": "1e-7"}`). Note that JS sets come back from ElectroDB as arrays (`L`).
- Errors are `{name, message, code?, fields?, cause?}`. `code` is ElectroDB's numeric code, kept for
  reference only; wattdb doesn't expose numeric codes (D48).
