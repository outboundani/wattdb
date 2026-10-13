package taskapp_test

import (
	"bytes"
	"context"
	"errors"
	"log/slog"
	"strings"
	"testing"

	"example.com/taskapp/db/member"
	"example.com/taskapp/db/taskapp"
	"example.com/taskapp/internal/vectest"
	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb"
	"github.com/outboundani/wattdb"
)

func TestHooks(t *testing.T) {
	client, fake := vectest.Client(nil)
	var calls []wattdb.Call
	var logs bytes.Buffer
	cfg := wattdb.NewConfig(client,
		wattdb.WithHooks(func(ctx context.Context, c wattdb.Call, next func(context.Context) (any, error)) (any, error) {
			calls = append(calls, c)
			if in, ok := c.Input.(*dynamodb.GetItemInput); ok {
				in.ConsistentRead = aws.Bool(true) // hooks may change the request
			}
			return next(ctx)
		}),
		wattdb.WithLogger(slog.New(slog.NewTextHandler(&logs, &slog.HandlerOptions{Level: slog.LevelDebug}))),
	)
	m, err := member.New(cfg, "table")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := m.Get(member.Key{OrgID: "O1", MemberID: "M1"}).Exec(ctx); !errors.Is(err, wattdb.ErrNotFound) {
		t.Fatal(err)
	}
	team, _ := taskapp.NewTeam(cfg, "table")
	if _, _, err := team.Query(taskapp.TeamKey{OrgID: "O1"}).Exec(ctx); err != nil {
		t.Fatal(err)
	}
	if _, _, err := m.BatchGet(member.Key{OrgID: "O1", MemberID: "M1"}).Exec(ctx); err != nil {
		t.Fatal(err)
	}
	want := []wattdb.Call{
		{Operation: "GetItem", Entity: "member", Table: "table"},
		{Operation: "Query", Table: "table"},
		{Operation: "BatchGetItem", Entity: "member"},
	}
	if len(calls) != len(want) {
		t.Fatalf("calls = %+v", calls)
	}
	for i, c := range calls {
		c.Input = nil
		if c != want[i] {
			t.Errorf("call %d = %+v, want %+v", i, c, want[i])
		}
	}
	if !strings.Contains(string(fake.Requests[0].Request), `"ConsistentRead":true`) {
		t.Errorf("hook change not sent: %s", fake.Requests[0].Request)
	}
	if n := strings.Count(logs.String(), "wattdb request"); n != 3 || !strings.Contains(logs.String(), "operation=GetItem entity=member") {
		t.Errorf("logs:\n%s", logs.String())
	}

	// A hook can answer without sending.
	blocked := wattdb.NewConfig(client, wattdb.WithHooks(func(ctx context.Context, c wattdb.Call, next func(context.Context) (any, error)) (any, error) {
		return nil, errors.New("blocked")
	}))
	bm, _ := member.New(blocked, "table")
	sent := len(fake.Requests)
	if _, err := bm.Get(member.Key{OrgID: "O1", MemberID: "M1"}).Exec(ctx); err == nil || err.Error() != "blocked" {
		t.Errorf("err = %v", err)
	}
	if len(fake.Requests) != sent {
		t.Error("blocked request was sent")
	}
}
