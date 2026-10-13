package vectest

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"strings"
	"sync"
	"testing"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb"
)

// Recorded is one case as recorded, for replaying through generated code.
type Recorded struct {
	Name      string
	Responses []json.RawMessage
	// Requests are what ElectroDB sent from .go() (or .params() when .go()
	// wasn't run).
	Requests []Request
	// Result is .go()'s wire-encoded result; Failed is set when it threw.
	Result json.RawMessage
	Failed bool
}

// Request is one recorded request.
type Request struct {
	Target  string          `json:"target"`
	Request json.RawMessage `json:"request"`
}

// LoadRecorded reads the cases of testdata/vectors/<name>.json by name.
func LoadRecorded(name string) (map[string]Recorded, error) {
	dir := os.Getenv("WATTDB_VECTORS")
	if dir == "" {
		_, file, _, _ := runtime.Caller(0)
		dir = filepath.Join(filepath.Dir(file), "../../../../../testdata/vectors")
	}
	b, err := os.ReadFile(filepath.Join(dir, name+".json"))
	if err != nil {
		return nil, err
	}
	var raw struct {
		Cases []struct {
			Name      string            `json:"name"`
			Responses []json.RawMessage `json:"responses"`
			Recorded  struct {
				Params json.RawMessage `json:"params"`
				Go     *struct {
					Result   json.RawMessage `json:"result"`
					Error    json.RawMessage `json:"error"`
					Requests []Request       `json:"requests"`
				} `json:"go"`
			} `json:"recorded"`
		} `json:"cases"`
	}
	if err := json.Unmarshal(b, &raw); err != nil {
		return nil, err
	}
	out := map[string]Recorded{}
	for _, rc := range raw.Cases {
		r := Recorded{Name: rc.Name, Responses: rc.Responses}
		if g := rc.Recorded.Go; g != nil {
			r.Requests, r.Result, r.Failed = g.Requests, g.Result, g.Error != nil
		} else {
			json.Unmarshal(rc.Recorded.Params, &r.Requests)
		}
		out[rc.Name] = r
	}
	return out, nil
}

// defaultResponses answer requests with no scripted response, as the
// recorder's transport does.
var defaultResponses = map[string]string{
	"BatchGetItem":   `{"Responses":{},"UnprocessedKeys":{}}`,
	"BatchWriteItem": `{"UnprocessedItems":{}}`,
	"Query":          `{"Items":[],"Count":0,"ScannedCount":0}`,
	"Scan":           `{"Items":[],"Count":0,"ScannedCount":0}`,
}

// Fake is an HTTP transport that answers DynamoDB calls from scripted
// responses and records the request bodies.
type Fake struct {
	mu        sync.Mutex
	responses []json.RawMessage
	Requests  []Request
}

// Client returns a DynamoDB client sending its requests to a Fake.
func Client(responses []json.RawMessage) (*dynamodb.Client, *Fake) {
	f := &Fake{responses: responses}
	c := dynamodb.New(dynamodb.Options{
		Region:           "us-east-1",
		Credentials:      aws.AnonymousCredentials{},
		BaseEndpoint:     aws.String("http://dynamodb.test"),
		HTTPClient:       f,
		RetryMaxAttempts: 1,
	})
	return c, f
}

// Do implements the SDK's HTTP client.
func (f *Fake) Do(req *http.Request) (*http.Response, error) {
	body, err := io.ReadAll(req.Body)
	if err != nil {
		return nil, err
	}
	target := strings.TrimPrefix(req.Header.Get("X-Amz-Target"), "DynamoDB_20120810.")
	f.mu.Lock()
	f.Requests = append(f.Requests, Request{Target: target, Request: body})
	var resp json.RawMessage
	if len(f.responses) > 0 {
		resp, f.responses = f.responses[0], f.responses[1:]
	} else if d, ok := defaultResponses[target]; ok {
		resp = json.RawMessage(d)
	} else {
		resp = json.RawMessage(`{}`)
	}
	f.mu.Unlock()
	status := http.StatusOK
	var probe struct {
		Type string `json:"__type"`
	}
	json.Unmarshal(resp, &probe)
	if probe.Type != "" {
		status = http.StatusBadRequest
	}
	return &http.Response{
		StatusCode: status,
		Header:     http.Header{"Content-Type": {"application/x-amz-json-1.0"}},
		Body:       io.NopCloser(bytes.NewReader(resp)),
		Request:    req,
	}, nil
}

// CheckRequests compares the requests sent with the recorded ones, as
// JSON. ignore names top-level fields to skip (such as a random token).
func CheckRequests(t *testing.T, got, want []Request, ignore ...string) {
	t.Helper()
	if len(got) != len(want) {
		t.Fatalf("sent %d requests, ElectroDB sent %d", len(got), len(want))
	}
	for i := range got {
		g, w := normalize(got[i].Request, ignore), normalize(want[i].Request, ignore)
		if got[i].Target != want[i].Target || !reflect.DeepEqual(g, w) {
			gb, _ := json.MarshalIndent(g, "", " ")
			wb, _ := json.MarshalIndent(w, "", " ")
			t.Errorf("request %d (%s, want %s) differs\n got: %s\nwant: %s", i, got[i].Target, want[i].Target, gb, wb)
		}
	}
}

func normalize(raw json.RawMessage, ignore []string) any {
	var v map[string]any
	if err := json.Unmarshal(raw, &v); err != nil {
		panic(fmt.Sprintf("bad request JSON: %v", err))
	}
	for _, k := range ignore {
		delete(v, k)
	}
	return v
}
