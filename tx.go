package wattdb

import (
	"context"
	"errors"
	"fmt"
	"strings"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb/internal/engine"
)

// ErrTxCanceled matches a canceled transaction; the error is a
// *TxCanceledError.
var ErrTxCanceled = errors.New("wattdb: transaction canceled")

// TxReason is why one transaction item failed, from DynamoDB's
// CancellationReasons.
type TxReason struct {
	// Code is DynamoDB's code: "None" for items that didn't fail,
	// "ConditionalCheckFailed", "TransactionConflict", ...
	Code    string
	Message string
	// Rejected is true when Code isn't "None".
	Rejected bool
	// Item is the existing item, decoded as the op's entity type (for
	// example user.User), when the op asked for it with TxReturnOld and
	// its condition failed; nil otherwise.
	Item any
}

// TxCanceledError reports a canceled transaction, with one reason per op in
// the order given (decision D42).
type TxCanceledError struct {
	Reasons []TxReason
	// Err is the SDK error.
	Err error
}

func (e *TxCanceledError) Error() string {
	var codes []string
	for _, r := range e.Reasons {
		codes = append(codes, r.Code)
	}
	return fmt.Sprintf("wattdb: transaction canceled [%s]", strings.Join(codes, ", "))
}

func (e *TxCanceledError) Unwrap() []error { return []error{ErrTxCanceled, e.Err} }

// TxOption configures a transaction write op.
type TxOption func(*engine.Options)

// TxReturnOld asks DynamoDB to return the existing item when the op's
// condition fails; it appears in TxCanceledError.Reasons.
func TxReturnOld() TxOption {
	return func(o *engine.Options) { o.Response = "all_old" }
}

// TxWriteOp is one write of a transaction, made by a builder's TxOp.
type TxWriteOp struct {
	build  func(exec bool) (*engine.Request, error)
	decode func(map[string]any) (any, error)
	err    error
}

func txWriteOp[E, P any](t *Table[E, P], c *call, opts []TxOption, build func(exec bool) (*engine.Request, error)) TxWriteOp {
	c.opts.Transaction = true
	for _, o := range opts {
		o(&c.opts)
	}
	return TxWriteOp{build: build, decode: func(m map[string]any) (any, error) { return t.decodeItem(m) }}
}

// TxOp makes the put a transaction op.
func (p *Put[E, P]) TxOp(opts ...TxOption) TxWriteOp {
	return txWriteOp(p.t, &p.c, opts, func(exec bool) (*engine.Request, error) {
		r, _, err := p.request(exec)
		return r, err
	})
}

// TxOp makes the delete a transaction op.
func (d *Delete[E, P]) TxOp(opts ...TxOption) TxWriteOp {
	return txWriteOp(d.t, &d.c, opts, func(exec bool) (*engine.Request, error) {
		r, _, err := d.request(exec)
		return r, err
	})
}

// TxOp makes the update a transaction op.
func (u *Update[E, P]) TxOp(opts ...TxOption) TxWriteOp {
	return txWriteOp(u.t, &u.c, opts, func(exec bool) (*engine.Request, error) {
		r, _, err := u.request(exec)
		return r, err
	})
}

// TxOp makes the upsert a transaction op.
func (u *Upsert[E, P]) TxOp(opts ...TxOption) TxWriteOp {
	return txWriteOp(u.t, &u.c, opts, func(exec bool) (*engine.Request, error) {
		r, _, err := u.request(exec)
		return r, err
	})
}

// TxCheck is a transaction condition check on one item.
type TxCheck[E, P any] struct {
	t   *Table[E, P]
	key map[string]types.AttributeValue
	err error
	c   call
}

// Check starts a condition check; generated clients call it with an
// encoded Key.
func (t *Table[E, P]) Check(key map[string]types.AttributeValue, err error) *TxCheck[E, P] {
	return &TxCheck[E, P]{t: t, key: key, err: err}
}

// Where adds the conditions to check.
func (c *TxCheck[E, P]) Where(conds ...Cond) *TxCheck[E, P] { c.c.where(conds); return c }

// Table overrides the table for this op.
func (c *TxCheck[E, P]) Table(name string) *TxCheck[E, P] { c.c.table = name; return c }

// TxOp makes the check a transaction op.
func (c *TxCheck[E, P]) TxOp(opts ...TxOption) TxWriteOp {
	return txWriteOp(c.t, &c.c, opts, func(exec bool) (*engine.Request, error) {
		if c.err != nil {
			return nil, c.err
		}
		return c.c.build(engine.NewCheck(c.t.def.model, engine.FromAttributeMap(c.key)), c.t.table, exec)
	})
}

// TxWriteCall is a write transaction being built.
type TxWriteCall struct {
	ctx    context.Context
	cfg    *Config
	ops    []TxWriteOp
	token  string
	client DynamoDB
	mutate []func(*dynamodb.TransactWriteItemsInput)
}

// TxWrite starts a write transaction of the ops, applied all or nothing
// (decision D42).
func TxWrite(ctx context.Context, cfg *Config, ops ...TxWriteOp) *TxWriteCall {
	return &TxWriteCall{ctx: ctx, cfg: cfg, ops: ops}
}

// Token sets the ClientRequestToken, making retries idempotent.
func (t *TxWriteCall) Token(id string) *TxWriteCall { t.token = id; return t }

// Client overrides the DynamoDB client.
func (t *TxWriteCall) Client(c DynamoDB) *TxWriteCall { t.client = c; return t }

// Mutate edits the request before it's sent.
func (t *TxWriteCall) Mutate(fn func(*dynamodb.TransactWriteItemsInput)) *TxWriteCall {
	t.mutate = append(t.mutate, fn)
	return t
}

func (t *TxWriteCall) request(exec bool) ([]*engine.Request, *dynamodb.TransactWriteItemsInput, error) {
	reqs := make([]*engine.Request, len(t.ops))
	in := &dynamodb.TransactWriteItemsInput{}
	for i, op := range t.ops {
		r, err := op.build(exec)
		if err != nil {
			return nil, nil, fmt.Errorf("wattdb: transaction op %d: %w", i, err)
		}
		reqs[i] = r
		p, err := toSDK(r)
		if err != nil {
			return nil, nil, err
		}
		in.TransactItems = append(in.TransactItems, p.txWriteItem(r.Target))
	}
	if t.token != "" {
		in.ClientRequestToken = aws.String(t.token)
	}
	for _, fn := range t.mutate {
		fn(in)
	}
	return reqs, in, nil
}

// Params returns the request without sending it.
func (t *TxWriteCall) Params() (*dynamodb.TransactWriteItemsInput, error) {
	_, in, err := t.request(false)
	return in, err
}

// Exec runs the transaction. A canceled transaction returns a
// *TxCanceledError.
func (t *TxWriteCall) Exec() error {
	reqs, in, err := t.request(true)
	if err != nil {
		return err
	}
	if len(reqs) == 0 {
		return nil
	}
	client := t.cfg.pick(t.client, "")
	if _, err := client.TransactWriteItems(t.ctx, in); err != nil {
		return txCanceled(err, len(reqs), func(i int, code, msg string, item map[string]any) (TxReason, error) {
			res, err := reqs[i].TxCanceledResult(code, msg, item)
			if err != nil {
				return TxReason{}, err
			}
			reason := TxReason{Code: res.Code, Message: res.Message, Rejected: res.Rejected}
			if res.Item != nil {
				if reason.Item, err = t.ops[i].decode(res.Item); err != nil {
					return TxReason{}, err
				}
			}
			return reason, nil
		})
	}
	return nil
}

// txCanceled converts a TransactionCanceledException; other errors are
// returned wrapped as usual.
func txCanceled(err error, n int, reason func(i int, code, msg string, item map[string]any) (TxReason, error)) error {
	var tce *types.TransactionCanceledException
	if !errors.As(err, &tce) {
		return wrapAWS(err)
	}
	out := &TxCanceledError{Err: err}
	for i := range n {
		var code, msg string
		var item map[string]any
		if i < len(tce.CancellationReasons) {
			cr := tce.CancellationReasons[i]
			code, msg = aws.ToString(cr.Code), aws.ToString(cr.Message)
			if cr.Item != nil {
				item = engine.FromAttributeMap(cr.Item)
			}
		}
		r, rerr := reason(i, code, msg, item)
		if rerr != nil {
			return rerr
		}
		out.Reasons = append(out.Reasons, r)
	}
	return out
}

// TxItem receives one item of a read transaction.
type TxItem[T any] struct {
	Item  T
	Found bool
}

// TxGetOp is one read of a read transaction, made by Get's TxGet.
type TxGetOp struct {
	build func(exec bool) (*engine.Request, error)
	set   func(map[string]any) error
}

// TxGet makes the get a read-transaction op that fills h.
func (g *Get[E, P]) TxGet(h *TxItem[E]) TxGetOp {
	return TxGetOp{
		build: func(exec bool) (*engine.Request, error) {
			r, _, err := g.request(exec)
			return r, err
		},
		set: func(m map[string]any) error {
			*h = TxItem[E]{}
			if m == nil {
				return nil
			}
			v, err := g.t.decodeItem(m)
			if err != nil {
				return err
			}
			*h = TxItem[E]{Item: v, Found: true}
			return nil
		},
	}
}

// TxGetCall is a read transaction being built.
type TxGetCall struct {
	ctx    context.Context
	cfg    *Config
	ops    []TxGetOp
	client DynamoDB
	mutate []func(*dynamodb.TransactGetItemsInput)
}

// TxGet starts a read transaction: a consistent read of every op's item
// (decision D42).
func TxGet(ctx context.Context, cfg *Config, ops ...TxGetOp) *TxGetCall {
	return &TxGetCall{ctx: ctx, cfg: cfg, ops: ops}
}

// Client overrides the DynamoDB client.
func (t *TxGetCall) Client(c DynamoDB) *TxGetCall { t.client = c; return t }

// Mutate edits the request before it's sent.
func (t *TxGetCall) Mutate(fn func(*dynamodb.TransactGetItemsInput)) *TxGetCall {
	t.mutate = append(t.mutate, fn)
	return t
}

func (t *TxGetCall) request(exec bool) ([]*engine.Request, *dynamodb.TransactGetItemsInput, error) {
	reqs := make([]*engine.Request, len(t.ops))
	in := &dynamodb.TransactGetItemsInput{}
	for i, op := range t.ops {
		r, err := op.build(exec)
		if err != nil {
			return nil, nil, fmt.Errorf("wattdb: transaction op %d: %w", i, err)
		}
		reqs[i] = r
		p, err := toSDK(r)
		if err != nil {
			return nil, nil, err
		}
		in.TransactItems = append(in.TransactItems, types.TransactGetItem{Get: &types.Get{
			TableName: aws.String(p.table), Key: p.key,
			ProjectionExpression: p.projection, ExpressionAttributeNames: p.names,
		}})
	}
	for _, fn := range t.mutate {
		fn(in)
	}
	return reqs, in, nil
}

// Params returns the request without sending it.
func (t *TxGetCall) Params() (*dynamodb.TransactGetItemsInput, error) {
	_, in, err := t.request(false)
	return in, err
}

// Exec runs the transaction and fills the ops' handles.
func (t *TxGetCall) Exec() error {
	reqs, in, err := t.request(true)
	if err != nil {
		return err
	}
	if len(reqs) == 0 {
		return nil
	}
	client := t.cfg.pick(t.client, "")
	out, err := client.TransactGetItems(t.ctx, in)
	if err != nil {
		return txCanceled(err, len(reqs), func(i int, code, msg string, _ map[string]any) (TxReason, error) {
			if code == "" {
				code = "None"
			}
			return TxReason{Code: code, Message: msg, Rejected: code != "None"}, nil
		})
	}
	for i, r := range reqs {
		var item map[string]any
		if i < len(out.Responses) && out.Responses[i].Item != nil {
			item = engine.FromAttributeMap(out.Responses[i].Item)
		}
		res, err := r.TxGetResult(item)
		if err != nil {
			return err
		}
		if err := t.ops[i].set(res.Item); err != nil {
			return err
		}
	}
	return nil
}
