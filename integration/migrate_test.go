//go:build integration

package integration

import (
	"context"
	"fmt"
	"testing"

	"github.com/outboundani/wattdb"
	"github.com/outboundani/wattdb/integration/db/task"
	"github.com/outboundani/wattdb/integration/db/taskv2"
)

func TestMigrateFrom(t *testing.T) {
	ctx := context.Background()
	table := newTableWith(t, nil)
	cfg := wattdb.NewConfig(ddb)
	v1, err := task.New(cfg, table)
	if err != nil {
		t.Fatal(err)
	}
	v2, err := taskv2.New(cfg, table)
	if err != nil {
		t.Fatal(err)
	}
	var ins []task.Input
	for i := range 40 {
		in := task.Input{ProjectID: "p1", TaskID: fmt.Sprintf("t%02d", i), Status: task.StatusOpen, Priority: ptr(i), CreatedAt: ptr("2026-10-12")}
		if i%2 == 0 {
			in.Assignee = ptr("ada")
		}
		ins = append(ins, in)
	}
	if left, err := v1.BatchPut(ins...).Exec(ctx); err != nil || len(left) > 0 {
		t.Fatalf("seed: %v, %d left", err, len(left))
	}

	n, err := taskv2.MigrateFrom(ctx, v1, v2, func(old task.Task) (taskv2.Input, error) {
		in := taskv2.Input{ProjectID: old.ProjectID, TaskID: old.TaskID, Title: old.Title, Status: taskv2.StatusEnum(old.Status)}
		if old.Priority != nil && *old.Priority >= 20 {
			in.Priority = ptr(taskv2.PriorityHigh)
		}
		if old.Assignee != nil {
			in.Assignees = []string{*old.Assignee}
		}
		return in, nil
	})
	if err != nil || n != 40 {
		t.Fatalf("migrated %d: %v", n, err)
	}

	got, err := v2.Get(taskv2.Key{ProjectID: "p1", TaskID: "t30"}).Exec(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if got.Priority != taskv2.PriorityHigh || len(got.Assignees) != 1 || got.Assignees[0] != "ada" {
		t.Errorf("t30 = %+v", got)
	}
	// Each version only sees its own items.
	olds, _, err := v1.Scan().AllPages().Exec(ctx)
	if err != nil || len(olds) != 40 {
		t.Fatalf("v1 scan: %d, %v", len(olds), err)
	}
	news, _, err := v2.Scan().AllPages().Exec(ctx)
	if err != nil || len(news) != 40 {
		t.Fatalf("v2 scan: %d, %v", len(news), err)
	}
}
