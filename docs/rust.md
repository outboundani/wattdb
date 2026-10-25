# wattdb for Rust

The Rust port reproduces the same ElectroDB 3.9 behavior as the Go library, checked against the same
recorded vectors in `testdata/vectors`. Everything in [design.md](design.md) and the compatibility
decisions (D1–D8, D56–D58) apply unchanged. This file covers what's different in Rust: how
schemas are written, the API shapes, and the few places where Rust's types change what can be
expressed.

## Layout

A Cargo workspace under `crates/`. Every dependency is declared once in the root `Cargo.toml` and
referenced with `dep.workspace = true`.

| Crate | Role | Published |
|---|---|---|
| `wattdb` | The typed runtime: entities, builders, batches, transactions, collections, hooks. Re-exports the derives and the engine (`wattdb::engine`). | yes |
| `wattdb-derive` | `#[derive(Entity, Map, Enum, Collection)]` | yes |
| `wattdb-engine` | The untyped ElectroDB engine (a port of `internal/engine` and `internal/model`): models, keys, items, expressions, cursors, conversions. No AWS dependency. | yes |
| `wattdb-vectors` | Loads the recorded vectors and their callback registry, for tests. | no |
| `wattdb-integration` | DynamoDB Local tests (the `integration/` package's port). | no |

## Decisions

These extend [decisions.md](decisions.md). Each one names the Go decision it replaces.

| # | Decision | Replaces |
|---|---|---|
| R1 | **Schemas are derives, not generated code.** `#[derive(Entity)]` on a struct generates `{Name}Input`, `{Name}Partial` (alias `{Name}Filter`), `{Name}Key`, `{Name}Client`, `{Name}Conv`, the sort-key step types and one descriptor function per attribute (`Task::status()`). There is no generation step, no `db/runtime` and no blank import. | D9, D10, D22, D24 |
| R2 | **The Rust field name is primary.** The stored name, which is also the ElectroDB attribute name, comes from `#[wattdb(field = "...")]` or the container's `rename_all` (serde's rules: `camelCase`, `snake_case`, `lowercase`, ...). Composites name Rust fields (`composite(org_id)`); templates accept either Rust or stored names (`"team#${org_id}"`, `"team#${orgId}"`). | D12, D59 |
| R3 | **Optional attributes are `Option<T>`; required ones are plain values.** Inputs follow D17: a required attribute without a default is a plain value, everything else an `Option` (`None` means "use the default"). | D16, D17 |
| R4 | **Callbacks are plain functions or closures named in the attribute**, typed by the field: `get`/`set` are `fn(Option<T>, &Partial) -> Option<T>`, `validate` is `fn(Option<&T>) -> Result<(), impl Display>`, an index `condition` is `fn(&Partial) -> bool`, and `default` is any expression converting into the field type. Map properties' callbacks get the map struct as siblings. | D11 |
| R5 | **`validate_regex` uses the `regex` crate's syntax** (RE2-like, as Go's: no lookaround) and is checked when the derive runs, so an invalid pattern is a compile error. | D61 |
| R6 | **Enums are Rust enums** with `#[derive(Enum)]`: `VALUES`, `as_str`, `Display`, `FromStr`. An optional `#[wattdb(other)] Other(String)` variant holds values no other variant names; without it, reading an unknown stored value is a decode error. `#[wattdb(serde)]` adds string serde impls. | D19 |
| R7 | **Codecs:** every type implements `FieldType` (its default storage), and `#[wattdb(codec = C)]` picks another storage, where `C: Codec<T>`. Defaults: `String` S; integers and floats N (exact text, plain decimals in keys per D56); `bool`; `Vec<T>` list; `BTreeSet`/`HashSet` sets (empty is missing); `DateTime<Utc>` UTC RFC 3339 with a 9-digit fraction; `Duration` nanoseconds; `Uuid` 16 bytes (B); `serde_json::Value` any. Overrides: `VecSet` (also `as_set`) for a `Vec` stored as a set, `JsIso` for `toISOString()` times, `UuidString`, `Binary` for `Vec<u8>`, `Serde` for any serde type (`AnyOf[T]`). A codec's encoded value is also its key text. | D18 |
| R8 | **Maps are structs** with `#[derive(Map)]`; their descriptor (`{Name}Attr<E>`) has one method per property, so `Item::meta().a()` and `Event::changes().at(0)` address nested paths. | D24, D60 |
| R9 | **Reads that find nothing return `Ok(None)`**: `get`, `delete(..).returning(AllOld)`, `returning_updated`, `parse`. There is no not-found error. | D28, D33, D46 |
| R10 | **Errors are one `wattdb::Error` enum** (`non_exhaustive`) with `ConditionFailed`, `Validation`, `IncompleteComposites`, `InvalidCursor`, `TxCanceled`, `Unprocessed`, `Sdk`, ... and `is_condition_failed()`. A canceled transaction's reasons carry their old items as `reason.item::<Project>()`. | D48, D42 |
| R11 | **wattdb runs on Tokio,** as the AWS SDK does; there's no runtime abstraction. Terminals are `async fn exec()`, and `pages()` and `all()` return `Stream`s. `exec()` returns a `Page { items, cursor, unprocessed }` (a `CollectionPage { result, .. }` for collections). Batch chunks run concurrently with `concurrent(n)`, and retries wait with `tokio::time::sleep`. | D31 |
| R12 | **Builder methods mirror the Go ones in snake case:** `where_` (a keyword), `matching` for ElectroDB's `match`, `tx_op()`/`tx_op_return_old()` for `TxOp(TxReturnOld())`, `return_rejected_with_old()` for `ReturnRejected(WithOld())`. Sort-key steps add `begins_with_prefix(&str)` for prefixes that aren't a whole value (an enum's first letters). | D29, D30, D37, D42 |
| R13 | **Transaction reads use slots:** `let mut tx = TxGet::new(&cfg); let a = tx.add(members.get(k).tx_get()); let out = tx.exec().await?; out.get(a)?` gives `Option<Member>`. | D42 |
| R14 | **Collections are structs** with `#[derive(Collection)]` and one `Vec` per member entity, in field order; the derive generates `{Name}Client` and `{Name}Key`. Sort-key steps come from the first member. | D21, D43 |
| R15 | **Hooks are closures** `Fn(Call, Next) -> Future<Output = Result<Response>>` added with `Config::with_hook`; `with_tracing()` adds the `tracing` logger (the `tracing` feature). `Call` carries the typed SDK input as `wattdb::Request`. | D50 |
| R16 | **Integration tests** live in `wattdb-integration` behind its `integration` feature and start the pinned `amazon/dynamodb-local` image with testcontainers' async runner (one container per test binary, a fresh table per test), or use the endpoint in `WATTDB_DYNAMODB_ENDPOINT`. Plain `cargo test` needs no Docker. All async tests are `#[tokio::test]`. | D63 |
| R17 | **Cargo features** (all default): `chrono` (`DateTime<Utc>`, `JsIso`), `uuid` (`Uuid`, `UuidString`), `json` (`serde_json::Value` attributes, `Serde`), `tracing` (`TracingHook`). | — |

## Differences from ElectroDB

Everything in [compat.md](compat.md) holds, with these Rust-specific notes:

| Case | ElectroDB | wattdb (Rust) |
|---|---|---|
| `null` for a string attribute | Rejected | Not expressible: `None` means missing |
| A required map property left unset | Rejected | Not expressible in an input decoded from JSON (it's a plain field); a `Default` value writes its zero value, as in Go (D17) |
| An enum value outside the list | Rejected on write | Not expressible unless the enum has an `other` variant; then rejected, as in ElectroDB |
| Wrong JSON types, unknown attributes | Coerced or dropped | Not expressible: inputs are typed structs |

## Testing

```sh
cargo test --workspace                                         # engine, derive and replayed vectors
cargo test -p wattdb-integration --features integration        # DynamoDB Local via testcontainers
WATTDB_DYNAMODB_ENDPOINT=http://localhost:8000 \
  cargo test -p wattdb-integration --features integration      # an already running DynamoDB Local
```

- `wattdb-engine`'s tests replay every vector through the engine: keys, put items and read formatting,
  writes, queries, batches, transactions, collections and conversions, with the same case counts
  as the Go engine tests.
- `wattdb`'s tests replay the "multi" vectors through derived entities and the real SDK client
  (with a fake HTTP layer) and compare the requests on the wire; run the "pipeline" and
  "callbacks" put/read cases through Rust callbacks; check that every vector entity, written with
  the derive, compiles to the vector's model; and cover hooks.
