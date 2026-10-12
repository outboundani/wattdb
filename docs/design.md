# wattdb design: ElectroDB compatibility contract

This document is the compatibility contract: how ElectroDB 3.9.3 behaves, taken from its source and
confirmed by running ElectroDB. wattdb must reproduce it (decisions D1, D3). Nothing here is a
choice; decisions live in [decisions.md](decisions.md).

---

The source references are to `../electrodb/src`.

### 1.1 Key construction for isolated (default) indexes

The prefixes come from `_makeKeyFixings` (`entity.js:3869`):

- **pk prefix:** `$<service>`
- **sk prefix:** `$<entity>_<version>`. If the index has collections, it is
  `$<col1>#<col2>...#<entity>_<version>`.
- **Index without an sk:** the sk prefix folds into the pk, giving `$<service>$<entity>_<version>`.
- **`scope`:** appended to the pk prefix as `_<scope>`.
- **Composites:** each one appends `#<label>_<value>`. The label is the attribute's `label`, or its
  name if it has none.
- **Casing:** applied to the whole key, values included. The default is lowercase; per key it can be
  `upper`, `lower` or `none`.
- **Padding:** applied to the string form of the value before casing.
- **Numbers** are formatted the way JS `${n}` does (`1e+21`, `0.000001`). Booleans become
  `true`/`false`.

Verified with ElectroDB:

```
pk     $taskapp#o_acme                    sk     $user_1#id_abc
gsi1sk $c1#c2#user_1#n_00042#b_true       (sub-collections, padding, boolean)
gsi3pk $taskapp$user_1#id_abc             (index without an sk)
```

Partial sort keys:

- Values passed to an access pattern count as "known", so the next label is appended
  (`...#n_00042#b_`).
- `.begins()` does not append the next label.
- A composite left out stops the key at that point.

### 1.2 Clustered indexes

The `$entity_version` part becomes a **postfix**, appended only when every sk composite is present:
`$cc#id_abc#user_1`. Partial clustered queries add an identifier `FilterExpression`. Clustered
collection queries leave out the postfix.

### 1.3 Templates and attributes used as key fields

- **`template`:** the key is built only from the template, with no service/entity/version prefixes.
  Casing still applies. Verified examples:
  - `user#${userId}` produces `user#abc` (default lowercase)
  - `task#${team}#${taskId}` with `casing: "none"` produces `task#Core#T-1`
- **Attribute used as a key field:** when an index `field` equals an attribute's field and that key
  has a single composite, ElectroDB rewrites it as the template `${field}` (`_optimizeIndexKey`).
- **Numeric attributes:** a custom key with exactly one numeric composite and no labels is stored as
  a **number** (`cast: number`).
- **String attributes:** the key casing is applied to the stored attribute value itself. Verified:
  `put({userId: "AbC"})` with `pk: {field: "userId", composite: ["userId"]}` stores
  `userId: "abc"`.

### 1.4 Items

- **`put`/`create`:** write the translated fields (`field` renames, including nested map fields),
  every index key, and `__edb_e__` (the original-case entity name) and `__edb_v__`.
- **Index keys on writes:**
  - Every write rebuilds each index key whose composites changed.
  - An update that would leave a key half-built is rejected, unless the missing composite is
    supplied through `.composite()` or comes from the primary key.
  - A sparse index whose `condition` returns false has its keys REMOVEd.
- **Updates:**
  - Every update also SETs the primary-key composite attributes and both identifiers, so an
    update that creates an item still produces a complete one.
  - `create` adds `attribute_not_exists(pk)` (and sk) to the condition.
  - `patch` and `remove` add `attribute_exists`.
  - `upsert` wraps readOnly attributes that aren't key composites in `if_not_exists`.
- **Attribute rules:**
  - **Sets:** deduplicated; an empty set means the attribute is absent.
  - **Maps:** an empty, optional root map with no default is written as absent.
  - **Write order:** `default`, then `set` (watchers run after the attributes they watch), then
    validation.
  - **Read order:** `get` (watchers last), then `hidden` attributes are removed.

### 1.5 Reads and ownership

- **Ownership filtering:** items are dropped unless `__edb_e__`/`__edb_v__` match. This is skipped
  with `ignoreOwnership`, and turned off automatically for `keys_only` indexes and for INCLUDE
  projections that don't project the identifiers. In those cases ownership falls back to checking
  the key structure.
- **Scans** add identifier filters plus `begins_with` on the key prefixes.
- **Collections** OR together `(e = :e AND v = :v)` for each member entity and sort items into
  entities by their identifiers.

### 1.6 Cursors

A cursor is `base64url(JSON.stringify(LastEvaluatedKey))`, where the key is in its unmarshalled
DocumentClient form. A cursor passed back in is trimmed to the index's key fields plus the table's
key fields.
