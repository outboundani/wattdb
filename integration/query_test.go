//go:build integration

package integration

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"reflect"
	"slices"
	"testing"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb"
	"github.com/outboundani/wattdb/integration/db/task"
)

func taskTable(t *testing.T) (*task.Client, string) {
	t.Helper()
	table := newTableWith(t, []gsiSpec{
		{name: "gsi1", pk: "gsi1pk", sk: "gsi1sk"},
		{name: "gsi2", pk: "gsi2pk", sk: "gsi2sk", projection: types.ProjectionTypeKeysOnly},
		{name: "gsi3", pk: "gsi3pk", sk: "gsi3sk"},
		{name: "gsi4", pk: "gsi4pk", sk: "gsi4sk", projection: types.ProjectionTypeInclude, include: []string{"title"}},
	})
	c, err := task.New(wattdb.NewConfig(ddb), table)
	if err != nil {
		t.Fatal(err)
	}
	return c, table
}

func i(n int) *int { return &n }

// seed writes tasks T1..T5 in project P1 and one in P2, plus a foreign
// item shaped like a task.
func seed(t *testing.T, c *task.Client, table string) {
	t.Helper()
	ctx := context.Background()
	tasks := []task.Input{
		{ProjectID: "P1", TaskID: "T1", Title: str("Alpha"), Status: task.StatusOpen, Priority: i(2), CreatedAt: str("2026-01-01"), Assignee: str("U1"), Secret: str("s")},
		{ProjectID: "P1", TaskID: "T2", Title: str("Beta"), Status: task.StatusOpen, Priority: i(5), CreatedAt: str("2026-01-02"), Assignee: str("U1")},
		{ProjectID: "P1", TaskID: "T3", Title: str("Gamma"), Status: task.StatusDone, Priority: i(1), CreatedAt: str("2026-01-03")},
		{ProjectID: "P1", TaskID: "T4", Title: str("Delta"), Status: task.StatusOpen, Priority: i(9), CreatedAt: str("2026-01-04")},
		{ProjectID: "P1", TaskID: "T5", Title: str("Epsilon"), Status: task.StatusDone, Priority: i(3), CreatedAt: str("2026-01-05")},
		{ProjectID: "P2", TaskID: "T6", Title: str("Other"), Status: task.StatusOpen, Priority: i(1), CreatedAt: str("2026-01-06")},
	}
	for _, in := range tasks {
		if _, err := c.Put(in).Exec(ctx); err != nil {
			t.Fatal(err)
		}
	}
	// A foreign entity's item in the same partition.
	_, err := ddb.PutItem(ctx, &dynamodb.PutItemInput{TableName: aws.String(table), Item: map[string]types.AttributeValue{
		"pk":        &types.AttributeValueMemberS{Value: "$taskapp#projectid_p1"},
		"sk":        &types.AttributeValueMemberS{Value: "$task_1#taskid_t9"},
		"projectId": &types.AttributeValueMemberS{Value: "P1"},
		"taskId":    &types.AttributeValueMemberS{Value: "T9"},
		"__edb_e__": &types.AttributeValueMemberS{Value: "other"},
		"__edb_v__": &types.AttributeValueMemberS{Value: "1"},
	}})
	if err != nil {
		t.Fatal(err)
	}
}

func ids(items []task.Task) []string {
	out := make([]string, len(items))
	for i, it := range items {
		out[i] = it.TaskID
	}
	return out
}

func expectIDs(t *testing.T, what string, items []task.Task, err error, want ...string) {
	t.Helper()
	if err != nil {
		t.Fatalf("%s: %v", what, err)
	}
	if got := ids(items); !slices.Equal(got, want) {
		t.Errorf("%s = %v, want %v", what, got, want)
	}
}

func TestQueries(t *testing.T) {
	ctx := context.Background()
	c, table := taskTable(t)
	seed(t, c, table)

	items, _, err := c.Primary("P1").Exec(ctx)
	expectIDs(t, "primary (foreign item dropped)", items, err, "T1", "T2", "T3", "T4", "T5")
	if items[0].Secret != nil {
		t.Error("hidden attribute returned")
	}
	items, _, err = c.Primary("P1").TaskID().Eq("T3").Exec(ctx)
	expectIDs(t, "full key", items, err, "T3")
	items, _, err = c.ByStatus("P1").Status().Eq(task.StatusOpen).Exec(ctx)
	expectIDs(t, "partial sk", items, err, "T1", "T2", "T4")
	// The default "keys" comparison compares built key strings, as
	// ElectroDB does: "...#priority_00002#createdat_..." sorts after
	// "...#priority_00002", so gt(2) includes T1 and between(2, 5)
	// excludes T2; lte(5) doesn't bound status, so "done" items (which sort
	// before "open") match too.
	items, _, err = c.ByStatus("P1").Status().Eq(task.StatusOpen).Priority().Gt(2).Exec(ctx)
	expectIDs(t, "gt", items, err, "T1", "T2", "T4")
	items, _, err = c.ByStatus("P1").Status().Eq(task.StatusOpen).Priority().Between(2, 5).Exec(ctx)
	expectIDs(t, "between", items, err, "T1")
	items, _, err = c.ByStatus("P1").Status().Eq(task.StatusOpen).Priority().Lte(5).Desc().Exec(ctx)
	expectIDs(t, "lte desc", items, err, "T1", "T5", "T3")
	// With compare attributes, the composites are also filtered.
	items, _, err = c.ByStatus("P1").Status().Eq(task.StatusOpen).Priority().Gt(2).Compare(wattdb.CompareAttributes).Exec(ctx)
	expectIDs(t, "gt with compare attributes", items, err, "T2", "T4")
	items, _, err = c.ByStatus("P1").Status().Eq(task.StatusOpen).Priority().Lte(5).Compare(wattdb.CompareAttributes).Exec(ctx)
	expectIDs(t, "lte with compare attributes", items, err, "T1")
	items, _, err = c.ByStatus("P1").Status().BeginsWith("d").Exec(ctx)
	expectIDs(t, "begins on the first composite", items, err, "T3", "T5")
	items, _, err = c.ByStatus("P1").Where(task.Title.BeginsWith("G")).Exec(ctx)
	expectIDs(t, "filter", items, err, "T3")
	items, _, err = c.ByStatusCluster(task.StatusDone).Exec(ctx)
	expectIDs(t, "clustered", items, err, "T3", "T5")

	// Compare attributes: keys compare "priority_00005" against
	// "priority_00002#createdat_..." strings; attributes also filter.
	items, _, err = c.ByStatus("P1").Status().Eq(task.StatusOpen).Priority().Gte(5).Compare(wattdb.CompareAttributes).Exec(ctx)
	expectIDs(t, "gte with compare attributes", items, err, "T2", "T4")

	parts, _, err := c.ByStatus("P1").Status().Eq(task.StatusDone).Attributes(task.Title).Exec(ctx)
	if err != nil || len(parts) != 2 || parts[0].Title == nil || parts[0].TaskID != nil {
		t.Errorf("Attributes = %+v, %v", parts, err)
	}
	keyed, _, err := c.Primary("P1").TaskID().Eq("T1").IncludeKeys().Exec(ctx)
	if err != nil || len(keyed) != 1 || keyed[0].Keys["pk"] == nil || keyed[0].Keys["__edb_e__"] == nil {
		t.Errorf("IncludeKeys = %+v, %v", keyed, err)
	}
	outs, err := c.Primary("P1").Raw().Exec(ctx)
	if err != nil || len(outs) != 1 || len(outs[0].Items) != 6 {
		t.Errorf("Raw = %d pages, %v", len(outs), err)
	}
}

func TestPagination(t *testing.T) {
	ctx := context.Background()
	c, table := taskTable(t)
	seed(t, c, table)

	// Limit 2 per request: one page by default, with a cursor.
	items, cur, err := c.Primary("P1").Limit(2).Exec(ctx)
	expectIDs(t, "first page", items, err, "T1", "T2")
	if cur == "" {
		t.Fatal("no cursor after the first page")
	}
	items, cur, err = c.Primary("P1").Limit(2).Cursor(cur).Exec(ctx)
	expectIDs(t, "second page", items, err, "T3", "T4")
	items, _, err = c.Primary("P1").Limit(2).AllPages().Exec(ctx)
	expectIDs(t, "all pages", items, err, "T1", "T2", "T3", "T4", "T5")
	items, cur, err = c.Primary("P1").Limit(2).Count(3).Exec(ctx)
	expectIDs(t, "count", items, err, "T1", "T2", "T3")
	items, _, err = c.Primary("P1").Cursor(cur).AllPages().Exec(ctx)
	expectIDs(t, "after count", items, err, "T4", "T5")

	var pages [][]string
	for page, err := range c.Primary("P1").Limit(2).Pages(ctx) {
		if err != nil {
			t.Fatal(err)
		}
		pages = append(pages, ids(page.Items))
	}
	if len(pages) < 3 || !slices.Equal(pages[0], []string{"T1", "T2"}) {
		t.Errorf("Pages = %v", pages)
	}
	var all []string
	for it, err := range c.ByStatus("P1").Limit(1).All(ctx) {
		if err != nil {
			t.Fatal(err)
		}
		all = append(all, it.TaskID)
	}
	if len(all) != 5 {
		t.Errorf("All = %v", all)
	}

	// A cursor in ElectroDB's own form (JSON keys in DynamoDB's order).
	js, _ := json.Marshal(map[string]string{"pk": "$taskapp#projectid_p1", "sk": "$task_1#taskid_t2"})
	items, _, err = c.Primary("P1").Cursor(wattdb.Cursor(base64.RawURLEncoding.EncodeToString(js))).AllPages().Exec(ctx)
	expectIDs(t, "ElectroDB cursor", items, err, "T3", "T4", "T5")
	if _, _, err := c.Primary("P1").Cursor("%%%").Exec(ctx); !errors.Is(err, wattdb.ErrInvalidCursor) {
		t.Errorf("bad cursor: %v", err)
	}
}

func TestIndexProjections(t *testing.T) {
	ctx := context.Background()
	c, table := taskTable(t)
	seed(t, c, table)

	// A keys-only index can't check identifiers; items come back empty
	// unless hydrated.
	items, _, err := c.ByAssignee("U1").Exec(ctx)
	if err != nil || len(items) != 2 || items[0].TaskID != "" {
		t.Errorf("keys-only query = %+v, %v", items, err)
	}
	items, _, err = c.ByAssignee("U1").Hydrate().Exec(ctx)
	expectIDs(t, "hydrated", items, err, "T1", "T2")
	if items[0].Title == nil || *items[0].Title != "Alpha" {
		t.Errorf("hydrated item = %+v", items[0])
	}
	items, _, err = c.ByTitle("P1").Exec(ctx)
	if err != nil || len(items) != 5 || items[0].Title == nil {
		t.Errorf("include-projection query = %+v, %v", items, err)
	}
}

func TestScans(t *testing.T) {
	ctx := context.Background()
	c, table := taskTable(t)
	seed(t, c, table)

	items, _, err := c.Scan().AllPages().Exec(ctx)
	if err != nil || len(items) != 6 {
		t.Fatalf("Scan = %v, %v", ids(items), err)
	}
	items, _, err = c.Scan().Where(task.Status.Eq(task.StatusDone)).AllPages().Exec(ctx)
	if got := ids(items); err != nil || len(got) != 2 {
		t.Errorf("filtered scan = %v, %v", got, err)
	}
	items, _, err = c.ScanByStatusCluster().AllPages().Exec(ctx)
	if err != nil || len(items) != 6 {
		t.Errorf("index scan = %v, %v", ids(items), err)
	}
	in, err := c.Scan().Limit(3).Params()
	if err != nil || *in.Limit != 3 || !reflect.DeepEqual(in.ExpressionAttributeNames["#pk"], "pk") {
		t.Errorf("Params = %+v, %v", in, err)
	}
}
