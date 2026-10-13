package wattdb

import (
	"context"
	"log/slog"
	"time"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb"
)

// Call describes one DynamoDB request a hook sees (decision D50).
type Call struct {
	// Operation is the DynamoDB operation: GetItem, Query, BatchWriteItem,
	// TransactWriteItems, ...
	Operation string
	// Entity is the entity making the request; empty for transactions and
	// collection queries, which can span entities.
	Entity string
	// Table is the request's table; empty for batches and transactions,
	// which name their tables per item.
	Table string
	// Input is the SDK input, such as *dynamodb.GetItemInput. Hooks may
	// change it before calling next.
	Input any
}

// Hook runs around every request: it may inspect or change the call, call
// next to send it (or not), and inspect or replace the result. The output
// is the SDK output, such as *dynamodb.GetItemOutput. A hook that answers
// without calling next must return an output of that type.
type Hook func(ctx context.Context, call Call, next func(ctx context.Context) (any, error)) (any, error)

// WithHooks adds hooks; the first one added is the outermost.
func WithHooks(hooks ...Hook) ConfigOption {
	return func(c *Config) { c.hooks = append(c.hooks, hooks...) }
}

// WithLogger logs every request to l (see SlogHook).
func WithLogger(l *slog.Logger) ConfigOption {
	return WithHooks(SlogHook(l))
}

// SlogHook logs each request at debug level, and failed ones at error
// level, with the operation, entity, table and duration.
func SlogHook(l *slog.Logger) Hook {
	return func(ctx context.Context, call Call, next func(ctx context.Context) (any, error)) (any, error) {
		start := time.Now()
		out, err := next(ctx)
		attrs := []slog.Attr{slog.String("operation", call.Operation)}
		if call.Entity != "" {
			attrs = append(attrs, slog.String("entity", call.Entity))
		}
		if call.Table != "" {
			attrs = append(attrs, slog.String("table", call.Table))
		}
		attrs = append(attrs, slog.Duration("duration", time.Since(start)))
		if err != nil {
			l.LogAttrs(ctx, slog.LevelError, "wattdb request failed", append(attrs, slog.Any("error", err))...)
		} else {
			l.LogAttrs(ctx, slog.LevelDebug, "wattdb request", attrs...)
		}
		return out, err
	}
}

// pick returns the client a request uses: the per-call override or the
// configured one, wrapped with the hooks.
func (c *Config) pick(override DynamoDB, entity string) DynamoDB {
	client := c.client
	if override != nil {
		client = override
	}
	if len(c.hooks) == 0 {
		return client
	}
	return &hooked{inner: client, hooks: c.hooks, entity: entity}
}

// hooked is a DynamoDB client running hooks around each call.
type hooked struct {
	inner  DynamoDB
	hooks  []Hook
	entity string
}

func run[I, O any](ctx context.Context, h *hooked, op string, table *string, in I, send func(context.Context, I) (O, error)) (O, error) {
	call := Call{Operation: op, Entity: h.entity, Table: aws.ToString(table), Input: in}
	var next func(i int) func(ctx context.Context) (any, error)
	next = func(i int) func(ctx context.Context) (any, error) {
		if i == len(h.hooks) {
			return func(ctx context.Context) (any, error) { return send(ctx, in) }
		}
		return func(ctx context.Context) (any, error) { return h.hooks[i](ctx, call, next(i+1)) }
	}
	out, err := next(0)(ctx)
	o, _ := out.(O)
	return o, err
}

func (h *hooked) GetItem(ctx context.Context, in *dynamodb.GetItemInput, opts ...func(*dynamodb.Options)) (*dynamodb.GetItemOutput, error) {
	return run(ctx, h, "GetItem", in.TableName, in, func(ctx context.Context, in *dynamodb.GetItemInput) (*dynamodb.GetItemOutput, error) {
		return h.inner.GetItem(ctx, in, opts...)
	})
}

func (h *hooked) PutItem(ctx context.Context, in *dynamodb.PutItemInput, opts ...func(*dynamodb.Options)) (*dynamodb.PutItemOutput, error) {
	return run(ctx, h, "PutItem", in.TableName, in, func(ctx context.Context, in *dynamodb.PutItemInput) (*dynamodb.PutItemOutput, error) {
		return h.inner.PutItem(ctx, in, opts...)
	})
}

func (h *hooked) DeleteItem(ctx context.Context, in *dynamodb.DeleteItemInput, opts ...func(*dynamodb.Options)) (*dynamodb.DeleteItemOutput, error) {
	return run(ctx, h, "DeleteItem", in.TableName, in, func(ctx context.Context, in *dynamodb.DeleteItemInput) (*dynamodb.DeleteItemOutput, error) {
		return h.inner.DeleteItem(ctx, in, opts...)
	})
}

func (h *hooked) UpdateItem(ctx context.Context, in *dynamodb.UpdateItemInput, opts ...func(*dynamodb.Options)) (*dynamodb.UpdateItemOutput, error) {
	return run(ctx, h, "UpdateItem", in.TableName, in, func(ctx context.Context, in *dynamodb.UpdateItemInput) (*dynamodb.UpdateItemOutput, error) {
		return h.inner.UpdateItem(ctx, in, opts...)
	})
}

func (h *hooked) Query(ctx context.Context, in *dynamodb.QueryInput, opts ...func(*dynamodb.Options)) (*dynamodb.QueryOutput, error) {
	return run(ctx, h, "Query", in.TableName, in, func(ctx context.Context, in *dynamodb.QueryInput) (*dynamodb.QueryOutput, error) {
		return h.inner.Query(ctx, in, opts...)
	})
}

func (h *hooked) Scan(ctx context.Context, in *dynamodb.ScanInput, opts ...func(*dynamodb.Options)) (*dynamodb.ScanOutput, error) {
	return run(ctx, h, "Scan", in.TableName, in, func(ctx context.Context, in *dynamodb.ScanInput) (*dynamodb.ScanOutput, error) {
		return h.inner.Scan(ctx, in, opts...)
	})
}

func (h *hooked) BatchGetItem(ctx context.Context, in *dynamodb.BatchGetItemInput, opts ...func(*dynamodb.Options)) (*dynamodb.BatchGetItemOutput, error) {
	return run(ctx, h, "BatchGetItem", nil, in, func(ctx context.Context, in *dynamodb.BatchGetItemInput) (*dynamodb.BatchGetItemOutput, error) {
		return h.inner.BatchGetItem(ctx, in, opts...)
	})
}

func (h *hooked) BatchWriteItem(ctx context.Context, in *dynamodb.BatchWriteItemInput, opts ...func(*dynamodb.Options)) (*dynamodb.BatchWriteItemOutput, error) {
	return run(ctx, h, "BatchWriteItem", nil, in, func(ctx context.Context, in *dynamodb.BatchWriteItemInput) (*dynamodb.BatchWriteItemOutput, error) {
		return h.inner.BatchWriteItem(ctx, in, opts...)
	})
}

func (h *hooked) TransactGetItems(ctx context.Context, in *dynamodb.TransactGetItemsInput, opts ...func(*dynamodb.Options)) (*dynamodb.TransactGetItemsOutput, error) {
	return run(ctx, h, "TransactGetItems", nil, in, func(ctx context.Context, in *dynamodb.TransactGetItemsInput) (*dynamodb.TransactGetItemsOutput, error) {
		return h.inner.TransactGetItems(ctx, in, opts...)
	})
}

func (h *hooked) TransactWriteItems(ctx context.Context, in *dynamodb.TransactWriteItemsInput, opts ...func(*dynamodb.Options)) (*dynamodb.TransactWriteItemsOutput, error) {
	return run(ctx, h, "TransactWriteItems", nil, in, func(ctx context.Context, in *dynamodb.TransactWriteItemsInput) (*dynamodb.TransactWriteItemsOutput, error) {
		return h.inner.TransactWriteItems(ctx, in, opts...)
	})
}
