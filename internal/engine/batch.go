package engine

import (
	"github.com/outboundani/wattdb/internal/model"
)

// Batch request limits, as in ElectroDB's MaxBatchItems.
const (
	MaxBatchGet   = 100
	MaxBatchWrite = 25
)

// BatchGetOptions shape a batch get.
type BatchGetOptions struct {
	Table           string
	Attributes      []string
	IgnoreOwnership bool
	PreserveOrder   bool
	// Exec is true when the request is sent rather than rendered.
	Exec bool
}

// BatchGet mirrors ElectroDB's batch get: params in chunks of 100, and
// response formatting with optional order preservation.
type BatchGet struct {
	m     *model.Model
	opt   BatchGetOptions
	table string
	// Requests are the BatchGetItem params, one per chunk.
	Requests []map[string]any

	slots map[string]int
	data  []map[string]any
	// Unprocessed are the unprocessed keys, formatted as composites.
	Unprocessed []Attrs
	// UnprocessedKeys are the same keys as DynamoDB returned them.
	UnprocessedKeys []map[string]any
}

// NewBatchGet builds the requests for a batch get of table-key composites.
func NewBatchGet(m *model.Model, table string, composites []Attrs, opt BatchGetOptions) (*BatchGet, error) {
	if opt.Table != "" {
		table = opt.Table
	}
	b := &BatchGet{m: m, opt: opt, table: table}
	keys, err := BatchGetKeys(m, composites)
	if err != nil {
		return nil, err
	}
	extra := map[string]any{}
	if len(opt.Attributes) > 0 {
		names := &orderedNames{}
		extra["ProjectionExpression"] = projection(m, names, opt.Attributes, opt.Exec && !opt.IgnoreOwnership)
		extra["ExpressionAttributeNames"] = names.m
	}
	for _, chunk := range chunks(keys, MaxBatchGet) {
		ks := make([]any, len(chunk))
		for i, k := range chunk {
			ks[i] = k
		}
		b.Requests = append(b.Requests, b.request(ks, extra))
	}
	if opt.PreserveOrder {
		b.slots = map[string]int{}
		for _, k := range keys {
			ks := b.keyString(k)
			if _, ok := b.slots[ks]; !ok {
				b.slots[ks] = len(b.slots)
			}
		}
		b.data = make([]map[string]any, len(b.slots))
	}
	return b, nil
}

func (b *BatchGet) request(keys []any, extra map[string]any) map[string]any {
	t := map[string]any{"Keys": keys}
	for k, v := range extra {
		t[k] = v
	}
	return map[string]any{"RequestItems": map[string]any{b.table: t}}
}

// Retry returns the request for unprocessed keys.
func (b *BatchGet) Retry(keys []map[string]any) map[string]any {
	ks := make([]any, len(keys))
	for i, k := range keys {
		ks[i] = k
	}
	extra := map[string]any{}
	if t, ok := b.Requests[0]["RequestItems"].(map[string]any)[b.table].(map[string]any); ok {
		for k, v := range t {
			if k != "Keys" {
				extra[k] = v
			}
		}
	}
	return b.request(ks, extra)
}

// Table is the table the requests target.
func (b *BatchGet) Table() string { return b.table }

func (b *BatchGet) keyString(item map[string]any) string {
	t := b.m.TableIndex()
	s := keyString(item[t.PK.Field])
	if t.HasSK {
		s += keyString(item[t.SK.Field])
	}
	return s
}

// Apply mirrors applyBulkGetResponseFormatting for one response: the items
// DynamoDB returned and its unprocessed keys. Callers serialize Apply.
func (b *BatchGet) Apply(items, unprocessed []map[string]any) error {
	for _, k := range unprocessed {
		b.UnprocessedKeys = append(b.UnprocessedKeys, k)
		b.Unprocessed = append(b.Unprocessed, KeysToItem(b.m, b.m.TableIndex(), k))
	}
	for _, item := range items {
		var data map[string]any
		if b.owns(item) {
			var err error
			if data, err = FormatItem(b.m, item, FormatOptions{Attributes: b.opt.Attributes}); err != nil {
				return err
			}
		}
		if slot, ok := b.slots[b.keyString(item)]; ok {
			b.data[slot] = data
		} else {
			b.data = append(b.data, data)
		}
	}
	return nil
}

// owns mirrors Entity.is for batch gets.
func (b *BatchGet) owns(item map[string]any) bool {
	if b.opt.IgnoreOwnership && len(b.opt.Attributes) > 0 && !itemIncludesKeys(b.m, item) {
		return true
	}
	if b.opt.IgnoreOwnership && ownsKeys(b.m, item) {
		return true
	}
	return OwnsItem(b.m, item)
}

// Data returns the formatted items: in key order with nil for missing ones
// when preserving order, otherwise in response order (nil for items the
// entity doesn't own).
func (b *BatchGet) Data() []map[string]any {
	if b.data == nil {
		return []map[string]any{}
	}
	return b.data
}

// BatchWriteOptions shape a batch write.
type BatchWriteOptions struct {
	Table string
}

// BatchWrite mirrors ElectroDB's batch put and delete: requests in chunks of
// 25 and unprocessed formatting.
type BatchWrite struct {
	m     *model.Model
	table string
	// Requests are the BatchWriteItem params, one per chunk.
	Requests []map[string]any
	// Unprocessed are the unprocessed writes, formatted: put items as read
	// items, delete keys as composites.
	Unprocessed []map[string]any
	// UnprocessedRequests are the same writes as DynamoDB returned them.
	UnprocessedRequests []map[string]any
}

// NewBatchPut builds a batch put of items.
func NewBatchPut(m *model.Model, table string, items []Attrs, opt BatchWriteOptions) (*BatchWrite, error) {
	b := newBatchWrite(m, table, opt)
	var reqs []map[string]any
	for _, it := range items {
		r, err := NewPut(m, it, false).Build(b.table, Options{})
		if err != nil {
			return nil, err
		}
		reqs = append(reqs, map[string]any{"PutRequest": map[string]any{"Item": r.Params["Item"]}})
	}
	b.chunk(reqs)
	return b, nil
}

// NewBatchDelete builds a batch delete of table-key composites.
func NewBatchDelete(m *model.Model, table string, keys []Attrs, opt BatchWriteOptions) (*BatchWrite, error) {
	b := newBatchWrite(m, table, opt)
	var reqs []map[string]any
	for _, k := range keys {
		r, err := NewDelete(m, k, false).Build(b.table, Options{})
		if err != nil {
			return nil, err
		}
		reqs = append(reqs, map[string]any{"DeleteRequest": map[string]any{"Key": r.Params["Key"]}})
	}
	b.chunk(reqs)
	return b, nil
}

func newBatchWrite(m *model.Model, table string, opt BatchWriteOptions) *BatchWrite {
	if opt.Table != "" {
		table = opt.Table
	}
	return &BatchWrite{m: m, table: table}
}

func (b *BatchWrite) chunk(reqs []map[string]any) {
	for _, c := range chunks(reqs, MaxBatchWrite) {
		b.Requests = append(b.Requests, b.Retry(c))
	}
}

// Retry returns the request for unprocessed writes.
func (b *BatchWrite) Retry(reqs []map[string]any) map[string]any {
	rs := make([]any, len(reqs))
	for i, r := range reqs {
		rs[i] = r
	}
	return map[string]any{"RequestItems": map[string]any{b.table: rs}}
}

// Table is the table the requests target.
func (b *BatchWrite) Table() string { return b.table }

// Apply mirrors formatBulkWriteResponse for one response's unprocessed
// writes. Callers serialize Apply.
func (b *BatchWrite) Apply(unprocessed []map[string]any) error {
	for _, req := range unprocessed {
		b.UnprocessedRequests = append(b.UnprocessedRequests, req)
		switch {
		case req["PutRequest"] != nil:
			put, _ := req["PutRequest"].(map[string]any)
			item, _ := put["Item"].(map[string]any)
			var data map[string]any
			if OwnsItem(b.m, item) {
				var err error
				if data, err = FormatItem(b.m, item, FormatOptions{}); err != nil {
					return err
				}
			}
			b.Unprocessed = append(b.Unprocessed, data)
		case req["DeleteRequest"] != nil:
			del, _ := req["DeleteRequest"].(map[string]any)
			key, _ := del["Key"].(map[string]any)
			b.Unprocessed = append(b.Unprocessed, KeysToItem(b.m, b.m.TableIndex(), key))
		}
	}
	return nil
}

func chunks[T any](s []T, n int) [][]T {
	var out [][]T
	for len(s) > n {
		out = append(out, s[:n])
		s = s[n:]
	}
	if len(s) > 0 {
		out = append(out, s)
	}
	return out
}
