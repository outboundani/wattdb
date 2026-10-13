package engine

// This file ports ElectroDB's transactions (transaction.js).

var txOperations = map[string]string{
	"GetItem":        "Get",
	"PutItem":        "Put",
	"UpdateItem":     "Update",
	"DeleteItem":     "Delete",
	"ConditionCheck": "ConditionCheck",
}

// TxParams renders TransactWriteItems or TransactGetItems params from
// requests built with Options.Transaction.
func TxParams(reqs []*Request, token string) map[string]any {
	items := make([]any, len(reqs))
	for i, r := range reqs {
		items[i] = map[string]any{txOperations[r.Target]: r.Params}
	}
	params := map[string]any{"TransactItems": items}
	if token != "" {
		params["ClientRequestToken"] = token
	}
	return params
}

// TxResult is one transaction item's outcome, as ElectroDB reports it.
type TxResult struct {
	// Item is the formatted item: a get's item, or the old item a failed
	// condition returned; nil when absent or not the entity's.
	Item     map[string]any
	Code     string
	Message  string
	Rejected bool
}

// TxGetResult mirrors cleanseTransactionData for one item.
func (r *Request) TxGetResult(item map[string]any) (TxResult, error) {
	if item == nil {
		return TxResult{}, nil
	}
	data, err := r.txItem(item)
	return TxResult{Item: data}, err
}

// TxCanceledResult mirrors cleanseCanceledData for one cancellation reason.
func (r *Request) TxCanceledResult(code, message string, item map[string]any) (TxResult, error) {
	if code == "" {
		code = "None"
	}
	res := TxResult{Code: code, Message: message, Rejected: code != "None"}
	if item != nil {
		data, err := r.txItem(item)
		if err != nil {
			return res, err
		}
		res.Item = data
	}
	return res, nil
}

// TxWriteResult is a committed write item's result.
func TxWriteResult() TxResult { return TxResult{Code: "None"} }

func (r *Request) txItem(item map[string]any) (map[string]any, error) {
	if !OwnsItem(r.w.m, item) {
		return nil, nil
	}
	return FormatItem(r.w.m, item, FormatOptions{IncludeKeys: r.opts.IncludeKeys, Attributes: r.opts.Attributes})
}
