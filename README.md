# wattdb

<img src="docs/assets/banner.svg" alt="wattdb">

wattdb is a strongly typed DynamoDB library for Go that reproduces
[ElectroDB](https://electrodb.dev) 3.9. You describe entities in Go and wattdb generates typed
clients for them. The items, keys, query results and cursors are the ones ElectroDB would produce,
so Go and TypeScript services can share a table while you port from one to the other.

```go
users, _ := user.New(cfg, "app-table")

u, err := users.Get(user.Key{OrgID: "acme", UserID: "u1"}).Exec(ctx)

err = users.Update(user.Key{OrgID: "acme", UserID: "u1"}).
	Set(user.Email, "ada@example.com").
	Add(user.LoginCount, 1).
	Where(user.Status.Eq(user.StatusActive)).
	Exec(ctx)

admins, cursor, err := users.ByRole("acme").Role().Eq(user.RoleAdmin).Limit(50).Exec(ctx)
```

- **Typed everything:** generated structs, keys, enums, attribute descriptors and sort-key steps.
  `user.Email.Gt(5)` doesn't compile.
- **ElectroDB compatible:** the same stored bytes, identifiers (`__edb_e__`/`__edb_v__`), ownership
  filtering, validation and callback order. Compatibility is checked against vectors recorded
  from ElectroDB itself.
- **The full feature set:** queries, scans, updates and upserts, conditions, projections,
  hydration, pagination, batches, transactions, collections (including clustered and
  sub-collections), composite indexes, find/match, conversions, parse, hooks and versioned
  schemas with migrations.

Requires Go 1.27 or later (the builders use generic methods) and the AWS SDK for Go v2.

## Getting started

```sh
go get github.com/outboundani/wattdb
```

### 1. Write a schema

A schema is any type with `Model`, `Attributes` and `Indexes` methods, usually one file per entity
under `schema/<service>/`:

```go
package schema

import (
	"github.com/outboundani/wattdb"
	"github.com/outboundani/wattdb/attr"
	"github.com/outboundani/wattdb/index"
)

var TaskApp = wattdb.Service("taskapp")

type Task struct{}

func (Task) Model() wattdb.Model {
	return wattdb.Model{Entity: "task", Version: "1", Service: TaskApp}
}

func (Task) Attributes() []wattdb.Attribute {
	return []wattdb.Attribute{
		attr.String("ProjectID").Field("projectId").Required(),
		attr.String("TaskID").Field("taskId").Required(),
		attr.String("Title").Field("title").Required(),
		attr.Enum("Status", "open", "done").Field("status").Required().Default("open"),
		attr.Int("Points").Field("points"),
		attr.String("Assignee").Field("assignee"),
	}
}

func (Task) Indexes() []wattdb.Index {
	return []wattdb.Index{
		index.Primary("primary").PKField("pk").PK("ProjectID").SKField("sk").SK("TaskID"),
		index.GSI("byAssignee", "gsi1").PKField("gsi1pk").PK("Assignee").SKField("gsi1sk").SK("Status", "TaskID"),
	}
}
```

The builders mirror ElectroDB's model options one to one: `.Field`, `.Label`, `.Default`,
`.Validate`, `.Get`/`.Set`, `.Watch`, `.ReadOnly`, `.Hidden`, `.Padding`, templates, casing,
casts, collections, clustered and composite indexes, sparse-index conditions and projections.
Callbacks are typed Go functions.

### 2. Generate

```go
//go:generate go run github.com/outboundani/wattdb/cmd/wattdb generate ./schema
```

This writes:
- `db/<entity>`: the client, item struct, `Input`, `Partial`, `Key`, enums and descriptors;
- `db/<service>`: collection clients;
- `db/runtime`: registers the schemas' callbacks.

`wattdb check ./schema` validates the schemas without writing anything.

### 3. Use it

```go
import (
	"example.com/app/db/task"
	_ "example.com/app/db/runtime" // registers the schemas
)

cfg := wattdb.NewConfig(dynamodb.NewFromConfig(awsCfg), wattdb.WithLogger(slog.Default()))
tasks, err := task.New(cfg, "app-table")

t, err := tasks.Create(task.Input{ProjectID: "p1", TaskID: "t1", Title: "Write docs"}).Exec(ctx)

for t, err := range tasks.ByAssignee("ada").Status().Eq(task.StatusOpen).All(ctx) {
	// every page, item by item
}
```

[`examples/taskapp`](examples/taskapp) is a complete program covering creates, batches, updates,
queries, transactions, collections and iterators. Run it against DynamoDB Local.

## A tour

```go
// Conditions and failures
_, err := tasks.Create(in).Exec(ctx) // errors.Is(err, wattdb.ErrConditionFailed) if it exists
out, err := tasks.Delete(k).ReturnRejected(wattdb.WithOld()).Exec(ctx)

// Queries: partition composites positionally, then sort-key steps
tasks.Primary("p1").TaskID().BeginsWith("t-2026").Desc().MaxPages(3).Exec(ctx)
tasks.Primary("p1").Where(wattdb.Or(task.Points.Gt(5), task.Assignee.NotExists())).Exec(ctx)
partials, _, err := tasks.Primary("p1").Attributes(task.Title, task.Status).Exec(ctx)

// Batches: chunked like ElectroDB, with optional concurrency and retries
found, missing, err := tasks.BatchGet(keys...).Concurrent(4).Exec(ctx)
left, err := tasks.BatchPut(inputs...).RetryUnprocessed(wattdb.Backoff(5, 50*time.Millisecond)).Exec(ctx)

// Transactions
err = wattdb.TxWrite(ctx, cfg,
	tasks.Update(k).Set(task.Status, task.StatusDone).TxOp(),
	projects.Check(pk).Where(project.Archived.NotExists()).TxOp(),
).Token(requestID).Exec()

// Collections: one query, one slice per entity
ws, _ := taskapp.NewWorkspace(cfg, "app-table")
r, cursor, err := ws.Query(taskapp.WorkspaceKey{ProjectID: "p1"}).Exec(ctx) // r.Projects, r.Tasks

// Find/Match pick the best index the way ElectroDB does
tasks.Match(task.Filter{ProjectID: ptr("p1"), Assignee: ptr("ada")}).Exec(ctx)

// Hooks wrap every request
cfg := wattdb.NewConfig(ddb, wattdb.WithHooks(func(ctx context.Context, c wattdb.Call, next func(context.Context) (any, error)) (any, error) {
	ctx, span := tracer.Start(ctx, c.Operation)
	defer span.End()
	return next(ctx)
}))
```

Every builder also has `.Params()` to see the request, `.Table(name)` and `.Client(c)` per call,
and `.Mutate(func(*dynamodb.XInput))` for anything else.

### Versioned schemas

Bumping `Version` gives the entity a new package (`db/taskv2`) next to the old one. ElectroDB
treats each version as a separate entity, and wattdb does too. The new package gets
`MigrateFrom`, which scans the old version, transforms each item and batch-writes the result:

```go
n, err := taskv2.MigrateFrom(ctx, tasksV1, tasksV2, func(old task.Task) (taskv2.Input, error) {
	return taskv2.Input{ProjectID: old.ProjectID, TaskID: old.TaskID, Title: old.Title}, nil
})
```

## Porting from ElectroDB

Translate each model attribute by attribute. Name it in Go and keep the stored name with
`.Field`:

```js
// ElectroDB
userId: { type: "string", required: true },
```

```go
// wattdb
attr.String("UserID").Field("userId").Required(),
```

Keep the entity name, version, service, labels, casing and templates exactly as they are, and the
keys will match. Time and UUID attributes need string codecs (`codec.JSISO`, `codec.UUIDString`)
if JS also reads them. [docs/compat.md](docs/compat.md) lists every place wattdb deliberately
differs, mostly inputs ElectroDB rejects.

## Documentation

- [docs/compat.md](docs/compat.md): every difference from ElectroDB.
- [docs/design.md](docs/design.md): the ElectroDB behavior wattdb reproduces.
- [docs/decisions.md](docs/decisions.md): the design decisions and their tradeoffs.

## Development

```sh
go test ./...                                      # unit and vector tests, no Docker needed
go test -tags integration ./integration/...        # DynamoDB Local via testcontainers
go generate ./integration/... ./examples/...       # regenerate the checked-in code
```

The compatibility oracle is in `testdata/vectors`: JSON recorded by running ElectroDB 3.9.3 against a
fake client ([tools/recordvectors](tools/recordvectors)). Node is only needed to re-record them.

## License

MIT
