# wattdb and ElectroDB: where they differ

wattdb reproduces ElectroDB 3.9.x (D2): the same keys, items, identifiers, query results, ownership
filtering and validation outcomes, so Go and JS services can share tables (D1). The behavior it
copies is specified in [design.md](design.md) and checked against recorded ElectroDB vectors (D8).

This file lists every deliberate difference (D3). Each one falls into one of these groups:

- inputs ElectroDB rejects, which wattdb accepts or can't express;
- edge values where the stored bytes differ;
- features left out;
- API shapes that return the same information differently.

Request placeholder names (`:status_u0`, `#pk`) are not part of the contract (D1), although in
practice they match too.

## Stored data

These are the only cases where wattdb can write bytes that ElectroDB wouldn't, or where its writes
read differently in JS.

| Case | ElectroDB | wattdb | Decision |
|---|---|---|---|
| Integers above 2^53 (any number attribute or numeric key) | Throws (`MAX_SAFE_INTEGER`) | Writes the exact integer. JS readers round it. | D7, D57 |
| Floats below `1e-6` or at least `1e21` in keys | `String(n)` exponent form (`1e-7`, `1e+21`) | Plain decimal (`0.0000001`). Override per field with a codec's `KeyString`. | D56 |
| Non-ASCII letters in key composites | Unicode `toLowerCase` | ASCII-only lowercasing, so `É` stays `É` | D6 |
| `-0` | `"0"` | `"0"` (same) | D58 |
| Empty set | Throws in the DocumentClient | Written as missing (an empty set is "absent") | D18 |
| A required map property left unset | Rejected | Written as its zero value: required properties are plain Go values | D17 |
| `null` for a string attribute | Rejected (validation error) | Not expressible: `nil` means missing, so the attribute is left out | D16, D17 |
| `time.Time` attributes | n/a (JS has no time type) | Default codec: UTC RFC 3339 with a 9-digit fraction. Use `codec.JSISO` for fields JS writes with `toISOString()`. | D18 |
| UUID attributes (`[16]byte`) | n/a | Default codec: binary (B), which ElectroDB can't read. Use `codec.UUIDString` for shared fields. | D18 |
| `time.Duration` attributes | n/a | Nanoseconds, as a number | D18 |

Everything else (prefixes, casing, padding, templates, clustered keys, collections, identifiers,
`if_not_exists` behavior, watcher order, validation results) matches byte for byte. The vector
tests enforce this.

## Behavior

| Case | ElectroDB | wattdb | Decision |
|---|---|---|---|
| Cursors | base64url JSON in DynamoDB's key order | base64url JSON with sorted keys. Each side decodes the other's, but the bytes can differ for the same position. | D32 |
| `ValidateRegexp` / `validate: RegExp` | JS regexp syntax | Go `regexp` (RE2) syntax. Ported patterns may need edits (no lookaround). | D61 |
| Compare mode `v2` | Supported (deprecated) | Not supported. `keys` (default) and `attributes` are. | D51 |
| Legacy "beta" models (root-level `service`/`entity`) | Supported | Not supported | D5 |
| Batch get of an item owned by another entity | `null` entry in `data` | Dropped (a `nil` slot with `PreserveOrder`) | D41 |
| Unprocessed batch work | Composites read back from the keys (lowercased) | The `Key`/`Input` values you passed in | D41 |
| Collection member order and aliases | The order and keys of `new Service({...})` | Generated package order, aliased by entity name. This only affects placeholder names and which entity claims an ambiguous item. | D43 |
| Collection `count` option | Accepted, but doesn't limit collection results | Not offered | D43 |
| `parse` of `Items`/`Attributes` responses | Supported | `Parse` takes one item | D46 |
| Event listeners and `logger` | Query/results events | Middleware hooks (`WithHooks`) and an slog adapter (`WithLogger`) | D50 |
| Error codes | Numeric `ElectroError` codes | Go sentinel errors and typed errors (`errors.Is`/`errors.As`) | D48 |
| `params` option merge | Plain object merge | Typed `.Mutate(func(*dynamodb.XInput))` | D40 |
| Migrations | None | Generated `MigrateFrom` between schema versions | D47 |
| Unprocessed-item retries | None | Opt-in `.RetryUnprocessed(...)` | D41 |

## Sharing tables with ElectroDB

- Keep the same entity name, version, service, field names, labels, casing and templates. Ported
  attributes usually need `.Field("camelName")` (D12).
- Use string codecs for times and UUIDs that JS also reads (`codec.JSISO`, `codec.UUIDString`).
- Keep integers that JS reads within ±2^53.
- Cursors work in both directions.
