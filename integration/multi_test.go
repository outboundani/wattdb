//go:build integration

package integration

import (
	"context"
	"errors"
	"fmt"
	"reflect"
	"slices"
	"testing"
	"time"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb"
	"github.com/outboundani/wattdb/integration/db/member"
	"github.com/outboundani/wattdb/integration/db/project"
	"github.com/outboundani/wattdb/integration/db/taskapp"
)

// multiTable creates a table for the member and project entities.
func multiTable(t *testing.T) string {
	t.Helper()
	return newTableWith(t, []gsiSpec{
		{name: "gsi1", pk: "gsi1pk", sk: "gsi1sk"},
		{name: "gsi2", pk: "gsi2pk", sk: "gsi2sk"},
		{name: "gsi3", pk: "gsi3pk", sk: "gsi3sk"},
	})
}

func ptr[T any](v T) *T { return &v }

func multiClients(t *testing.T, table string) (*wattdb.Config, *member.Client, *project.Client) {
	t.Helper()
	cfg := wattdb.NewConfig(ddb)
	m, err := member.New(cfg, table)
	if err != nil {
		t.Fatal(err)
	}
	p, err := project.New(cfg, table)
	if err != nil {
		t.Fatal(err)
	}
	return cfg, m, p
}

func TestBatches(t *testing.T) {
	ctx := context.Background()
	_, members, _ := multiClients(t, multiTable(t))

	var ins []member.Input
	var keys []member.Key
	for i := range 60 {
		id := fmt.Sprintf("M%02d", i)
		ins = append(ins, member.Input{OrgID: "O1", MemberID: id, Visits: ptr(i)})
		keys = append(keys, member.Key{OrgID: "O1", MemberID: id})
	}
	unprocessed, err := members.BatchPut(ins...).Concurrent(3).RetryUnprocessed(wattdb.Backoff(5, 10*time.Millisecond)).Exec(ctx)
	if err != nil || len(unprocessed) != 0 {
		t.Fatalf("BatchPut: %v, unprocessed %v", err, unprocessed)
	}

	// 60 stored plus 50 missing keys: two chunks.
	all := slices.Clone(keys)
	for i := range 50 {
		all = append(all, member.Key{OrgID: "O1", MemberID: fmt.Sprintf("X%02d", i)})
	}
	found, unprocessedKeys, err := members.BatchGet(all...).Concurrent(2).Exec(ctx)
	if err != nil || len(unprocessedKeys) != 0 {
		t.Fatalf("BatchGet: %v, unprocessed %v", err, unprocessedKeys)
	}
	if len(found) != 60 {
		t.Fatalf("found %d items, want 60", len(found))
	}

	ordered, _, err := members.BatchGet(keys[5], member.Key{OrgID: "O1", MemberID: "nope"}, keys[2]).PreserveOrder().Exec(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(ordered) != 3 || ordered[0].MemberID != "M05" || ordered[1] != nil || ordered[2].MemberID != "M02" {
		t.Fatalf("ordered = %+v", ordered)
	}
	if *ordered[0].Visits != 5 {
		t.Errorf("visits = %d", *ordered[0].Visits)
	}

	if unprocessedKeys, err := members.BatchDelete(keys[:30]...).Exec(ctx); err != nil || len(unprocessedKeys) != 0 {
		t.Fatalf("BatchDelete: %v, %v", err, unprocessedKeys)
	}
	found, _, err = members.BatchGet(keys...).Exec(ctx)
	if err != nil || len(found) != 30 {
		t.Fatalf("after delete: %v, %d items", err, len(found))
	}
}

func TestTransactions(t *testing.T) {
	ctx := context.Background()
	cfg, members, projects := multiClients(t, multiTable(t))

	err := wattdb.TxWrite(ctx, cfg,
		members.Put(member.Input{OrgID: "O1", MemberID: "M1", Name: ptr("Ada")}).TxOp(),
		projects.Create(project.Input{OrgID: "O1", ProjectID: "P1", Name: ptr("Apollo")}).TxOp(),
	).Token("tx-1").Exec()
	if err != nil {
		t.Fatal(err)
	}

	var m wattdb.TxItem[member.Member]
	var p, missing wattdb.TxItem[project.Project]
	if err := wattdb.TxGet(ctx, cfg,
		members.Get(member.Key{OrgID: "O1", MemberID: "M1"}).TxGet(&m),
		projects.Get(project.Key{OrgID: "O1", ProjectID: "P1"}).TxGet(&p),
		projects.Get(project.Key{OrgID: "O1", ProjectID: "P9"}).TxGet(&missing),
	).Exec(); err != nil {
		t.Fatal(err)
	}
	if !m.Found || *m.Item.Name != "Ada" || !p.Found || *p.Item.Name != "Apollo" || missing.Found {
		t.Fatalf("got %+v %+v %+v", m, p, missing)
	}

	// The check fails, so nothing is written; the create's old item comes
	// back with the reasons.
	err = wattdb.TxWrite(ctx, cfg,
		members.Update(member.Key{OrgID: "O1", MemberID: "M1"}).Set(member.Name, "Bo").TxOp(),
		members.Check(member.Key{OrgID: "O1", MemberID: "M1"}).Where(member.Visits.Exists()).TxOp(),
		projects.Create(project.Input{OrgID: "O1", ProjectID: "P1"}).TxOp(wattdb.TxReturnOld()),
	).Exec()
	var tce *wattdb.TxCanceledError
	if !errors.As(err, &tce) {
		t.Fatalf("err = %v, want TxCanceledError", err)
	}
	codes := []string{tce.Reasons[0].Code, tce.Reasons[1].Code, tce.Reasons[2].Code}
	if !reflect.DeepEqual(codes, []string{"None", "ConditionalCheckFailed", "ConditionalCheckFailed"}) {
		t.Fatalf("codes = %v", codes)
	}
	if old, ok := tce.Reasons[2].Item.(project.Project); !ok || *old.Name != "Apollo" {
		t.Errorf("old item = %#v", tce.Reasons[2].Item)
	}
	got, err := members.Get(member.Key{OrgID: "O1", MemberID: "M1"}).Exec(ctx)
	if err != nil || *got.Name != "Ada" {
		t.Fatalf("after canceled tx: %+v, %v", got, err)
	}
}

func TestCollections(t *testing.T) {
	ctx := context.Background()
	table := multiTable(t)
	cfg, members, projects := multiClients(t, table)
	for _, in := range []member.Input{
		{OrgID: "O1", MemberID: "M1", Name: ptr("Ada")},
		{OrgID: "O1", MemberID: "M2", Name: ptr("Bo"), Status: ptr(member.StatusAway)},
		{OrgID: "O2", MemberID: "M3", Name: ptr("Cy")},
	} {
		if _, err := members.Put(in).Exec(ctx); err != nil {
			t.Fatal(err)
		}
	}
	for _, in := range []project.Input{
		{OrgID: "O1", ProjectID: "P1", Name: ptr("Apollo")},
		{OrgID: "O1", ProjectID: "P2", Name: ptr("Zeus"), Status: ptr(project.StatusAway)},
	} {
		if _, err := projects.Put(in).Exec(ctx); err != nil {
			t.Fatal(err)
		}
	}

	team, err := taskapp.NewTeam(cfg, table)
	if err != nil {
		t.Fatal(err)
	}
	r, cur, err := team.Query(taskapp.TeamKey{OrgID: "O1"}).Exec(ctx)
	if err != nil || cur != "" {
		t.Fatalf("team: %v, cursor %q", err, cur)
	}
	if len(r.Members) != 2 || len(r.Projects) != 2 {
		t.Fatalf("team = %+v", r)
	}
	r, _, err = team.Query(taskapp.TeamKey{OrgID: "O1"}).Where(member.Name.Eq("Ada")).Exec(ctx)
	if err != nil || len(r.Members) != 1 || len(r.Projects) != 0 {
		t.Fatalf("filtered team = %+v, %v", r, err)
	}

	// Pages: one item per page.
	pages, total := 0, 0
	for page, err := range team.Query(taskapp.TeamKey{OrgID: "O1"}).Limit(1).Pages(ctx) {
		if err != nil {
			t.Fatal(err)
		}
		pages++
		total += len(page.Result.Members) + len(page.Result.Projects)
	}
	if total != 4 || pages < 4 {
		t.Errorf("pages = %d, items = %d", pages, total)
	}

	board, err := taskapp.NewBoard(cfg, table)
	if err != nil {
		t.Fatal(err)
	}
	b, _, err := board.Query(taskapp.BoardKey{OrgID: "O1"}).Status().Eq(member.StatusAway).Exec(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if len(b.Members) != 1 || b.Members[0].MemberID != "M2" || len(b.Projects) != 1 || b.Projects[0].ProjectID != "P2" {
		t.Fatalf("board away = %+v", b)
	}
	b, _, err = board.Query(taskapp.BoardKey{OrgID: "O1"}).Status().Eq(member.StatusAway).MemberID().Eq("M2").Exec(ctx)
	if err != nil || len(b.Members) != 1 || len(b.Projects) != 0 {
		t.Fatalf("board M2 = %+v, %v", b, err)
	}

	org, err := taskapp.NewOrgMembers(cfg, table)
	if err != nil {
		t.Fatal(err)
	}
	om, _, err := org.Query(taskapp.OrgMembersKey{OrgID: "O1"}).Exec(ctx)
	if err != nil || len(om.Members) != 2 {
		t.Fatalf("orgMembers = %+v, %v", om, err)
	}
	parent, err := taskapp.NewOrg(cfg, table)
	if err != nil {
		t.Fatal(err)
	}
	all, _, err := parent.Query(taskapp.OrgKey{OrgID: "O1"}).Exec(ctx)
	if err != nil || len(all.Members) != 2 || len(all.Projects) != 2 {
		t.Fatalf("org = %+v, %v", all, err)
	}
}

func TestFindAndMatch(t *testing.T) {
	ctx := context.Background()
	_, members, _ := multiClients(t, multiTable(t))
	for _, in := range []member.Input{
		{OrgID: "O1", MemberID: "M1", Name: ptr("Ada"), Visits: ptr(3)},
		{OrgID: "O1", MemberID: "M2", Name: ptr("Bo"), Visits: ptr(3)},
		{OrgID: "O2", MemberID: "M3", Name: ptr("Ada")},
	} {
		if _, err := members.Put(in).Exec(ctx); err != nil {
			t.Fatal(err)
		}
	}
	got, _, err := members.Find(member.Filter{OrgID: ptr("O1"), MemberID: ptr("M2")}).Exec(ctx)
	if err != nil || len(got) != 1 || got[0].MemberID != "M2" {
		t.Fatalf("find by key = %+v, %v", got, err)
	}
	got, _, err = members.Match(member.Filter{OrgID: ptr("O1"), Name: ptr("Ada")}).Exec(ctx)
	if err != nil || len(got) != 1 || got[0].MemberID != "M1" {
		t.Fatalf("match = %+v, %v", got, err)
	}
	got, _, err = members.Match(member.Filter{Name: ptr("Ada")}).AllPages().Exec(ctx)
	if err != nil || len(got) != 2 {
		t.Fatalf("match by scan = %+v, %v", got, err)
	}
}

func TestCompositeIndex(t *testing.T) {
	ctx := context.Background()
	table := fmt.Sprintf("table%d", tables.Add(1))
	_, err := ddb.CreateTable(ctx, &dynamodb.CreateTableInput{
		TableName:   aws.String(table),
		BillingMode: types.BillingModePayPerRequest,
		AttributeDefinitions: []types.AttributeDefinition{
			{AttributeName: aws.String("pk"), AttributeType: types.ScalarAttributeTypeS},
			{AttributeName: aws.String("sk"), AttributeType: types.ScalarAttributeTypeS},
			{AttributeName: aws.String("orgId"), AttributeType: types.ScalarAttributeTypeS},
			{AttributeName: aws.String("owner"), AttributeType: types.ScalarAttributeTypeS},
			{AttributeName: aws.String("due"), AttributeType: types.ScalarAttributeTypeS},
			{AttributeName: aws.String("projectId"), AttributeType: types.ScalarAttributeTypeS},
		},
		KeySchema: []types.KeySchemaElement{
			{AttributeName: aws.String("pk"), KeyType: types.KeyTypeHash},
			{AttributeName: aws.String("sk"), KeyType: types.KeyTypeRange},
		},
		GlobalSecondaryIndexes: []types.GlobalSecondaryIndex{{
			IndexName: aws.String("gsi4"),
			KeySchema: []types.KeySchemaElement{
				{AttributeName: aws.String("orgId"), KeyType: types.KeyTypeHash},
				{AttributeName: aws.String("owner"), KeyType: types.KeyTypeHash},
				{AttributeName: aws.String("due"), KeyType: types.KeyTypeRange},
				{AttributeName: aws.String("projectId"), KeyType: types.KeyTypeRange},
			},
			Projection: &types.Projection{ProjectionType: types.ProjectionTypeAll},
		}},
	})
	if err != nil {
		t.Skipf("DynamoDB Local doesn't support multi-attribute GSI keys: %v", err)
	}
	_, _, projects := multiClients(t, table)
	for _, in := range []project.Input{
		{OrgID: "O1", ProjectID: "P1", Owner: ptr("M1"), Due: ptr("2026-02-01")},
		{OrgID: "O1", ProjectID: "P2", Owner: ptr("M1"), Due: ptr("2026-05-01")},
		{OrgID: "O1", ProjectID: "P3", Owner: ptr("M2"), Due: ptr("2026-05-01")},
	} {
		if _, err := projects.Put(in).Exec(ctx); err != nil {
			t.Fatal(err)
		}
	}
	got, _, err := projects.ByOwner("O1", "M1").Exec(ctx)
	if err != nil || len(got) != 2 {
		t.Fatalf("by owner = %+v, %v", got, err)
	}
	got, _, err = projects.ByOwner("O1", "M1").Due().Gt("2026-03-01").Exec(ctx)
	if err != nil || len(got) != 1 || got[0].ProjectID != "P2" {
		t.Fatalf("by owner gt = %+v, %v", got, err)
	}
}
