package taskapp_test

import (
	"context"
	"errors"
	"reflect"
	"testing"

	"example.com/taskapp/db/member"
	"example.com/taskapp/db/project"
	_ "example.com/taskapp/db/runtime"
	"example.com/taskapp/db/taskapp"
	"example.com/taskapp/internal/vectest"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb"
)

// These tests replay the "multi" vectors through the generated clients and
// the real SDK client, comparing the requests on the wire with ElectroDB's.

var ctx = context.Background()

func load(t *testing.T, name string) (vectest.Recorded, *wattdb.Config, *vectest.Fake) {
	t.Helper()
	all, err := vectest.LoadRecorded("multi")
	if err != nil {
		t.Fatal(err)
	}
	rc, ok := all[name]
	if !ok {
		t.Fatalf("no case %q", name)
	}
	client, fake := vectest.Client(rc.Responses)
	return rc, wattdb.NewConfig(client), fake
}

// result decodes the recorded result's fields.
func result(t *testing.T, rc vectest.Recorded) map[string]types.AttributeValue {
	t.Helper()
	av, err := vectest.AttributeValue(rc.Result)
	if err != nil {
		t.Fatal(err)
	}
	return av.(*types.AttributeValueMemberM).Value
}

func list[T any](t *testing.T, av types.AttributeValue, decode func(map[string]types.AttributeValue) (T, error)) []T {
	t.Helper()
	var out []T
	for _, it := range av.(*types.AttributeValueMemberL).Value {
		v, err := decode(it.(*types.AttributeValueMemberM).Value)
		if err != nil {
			t.Fatal(err)
		}
		out = append(out, v)
	}
	return out
}

func members(t *testing.T, cfg *wattdb.Config) *member.Client {
	c, err := member.New(cfg, "table")
	if err != nil {
		t.Fatal(err)
	}
	return c
}

func projects(t *testing.T, cfg *wattdb.Config) *project.Client {
	c, err := project.New(cfg, "table")
	if err != nil {
		t.Fatal(err)
	}
	return c
}

func ptr[T any](v T) *T { return &v }

func TestCollections(t *testing.T) {
	type res struct {
		Members  []member.Member
		Projects []project.Project
	}
	for _, tt := range []struct {
		name string
		run  func(cfg *wattdb.Config) (res, wattdb.Cursor, error)
	}{
		{"isolated collection", func(cfg *wattdb.Config) (res, wattdb.Cursor, error) {
			c, _ := taskapp.NewTeam(cfg, "table")
			r, cur, err := c.Query(taskapp.TeamKey{OrgID: "O1"}).Exec(ctx)
			return res{r.Members, r.Projects}, cur, err
		}},
		{"isolated collection with a filter", func(cfg *wattdb.Config) (res, wattdb.Cursor, error) {
			c, _ := taskapp.NewTeam(cfg, "table")
			r, cur, err := c.Query(taskapp.TeamKey{OrgID: "O1"}).Where(member.Name.Eq("Ada")).Exec(ctx)
			return res{r.Members, r.Projects}, cur, err
		}},
		{"clustered collection", func(cfg *wattdb.Config) (res, wattdb.Cursor, error) {
			c, _ := taskapp.NewBoard(cfg, "table")
			r, cur, err := c.Query(taskapp.BoardKey{OrgID: "O1"}).Exec(ctx)
			return res{r.Members, r.Projects}, cur, err
		}},
		{"clustered collection by status", func(cfg *wattdb.Config) (res, wattdb.Cursor, error) {
			c, _ := taskapp.NewBoard(cfg, "table")
			r, cur, err := c.Query(taskapp.BoardKey{OrgID: "O1"}).Status().Eq(member.StatusAway).Exec(ctx)
			return res{r.Members, r.Projects}, cur, err
		}},
		{"clustered collection gt", func(cfg *wattdb.Config) (res, wattdb.Cursor, error) {
			c, _ := taskapp.NewBoard(cfg, "table")
			r, cur, err := c.Query(taskapp.BoardKey{OrgID: "O1"}).Status().Gt(member.StatusActive).Exec(ctx)
			return res{r.Members, r.Projects}, cur, err
		}},
		{"sub-collection parent", func(cfg *wattdb.Config) (res, wattdb.Cursor, error) {
			c, _ := taskapp.NewOrg(cfg, "table")
			r, cur, err := c.Query(taskapp.OrgKey{OrgID: "O1"}).Exec(ctx)
			return res{r.Members, r.Projects}, cur, err
		}},
		{"sub-collection child", func(cfg *wattdb.Config) (res, wattdb.Cursor, error) {
			c, _ := taskapp.NewOrgMembers(cfg, "table")
			r, cur, err := c.Query(taskapp.OrgMembersKey{OrgID: "O1"}).Exec(ctx)
			return res{Members: r.Members}, cur, err
		}},
		{"collection pages", func(cfg *wattdb.Config) (res, wattdb.Cursor, error) {
			c, _ := taskapp.NewTeam(cfg, "table")
			r, cur, err := c.Query(taskapp.TeamKey{OrgID: "O1"}).AllPages().Exec(ctx)
			return res{r.Members, r.Projects}, cur, err
		}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			rc, cfg, fake := load(t, tt.name)
			got, cur, err := tt.run(cfg)
			if err != nil {
				t.Fatal(err)
			}
			vectest.CheckRequests(t, fake.Requests, rc.Requests)
			data := result(t, rc)["data"].(*types.AttributeValueMemberM).Value
			var want res
			if av, ok := data["member"]; ok {
				want.Members = list(t, av, member.Decode)
			}
			if av, ok := data["project"]; ok {
				want.Projects = list(t, av, project.Decode)
			}
			if !reflect.DeepEqual(got, want) {
				t.Errorf("result = %+v, want %+v", got, want)
			}
			if cur != "" {
				t.Errorf("cursor = %q, want none", cur)
			}
		})
	}
}

func TestTransactions(t *testing.T) {
	t.Run("write transaction", func(t *testing.T) {
		rc, cfg, fake := load(t, "write transaction")
		m, p := members(t, cfg), projects(t, cfg)
		err := wattdb.TxWrite(ctx, cfg,
			m.Put(member.Input{OrgID: "O1", MemberID: "M1", Name: ptr("Ada")}).TxOp(),
			p.Update(project.Key{OrgID: "O1", ProjectID: "P1"}).Set(project.Name, "Zeus").Where(project.Status.Eq(project.StatusActive)).TxOp(),
			m.Delete(member.Key{OrgID: "O1", MemberID: "M2"}).TxOp(),
			m.Check(member.Key{OrgID: "O1", MemberID: "M3"}).Where(member.Name.Exists()).TxOp(),
			p.Create(project.Input{OrgID: "O1", ProjectID: "P2", Name: ptr("Hera")}).TxOp(wattdb.TxReturnOld()),
		).Token("req-1").Exec()
		if err != nil {
			t.Fatal(err)
		}
		vectest.CheckRequests(t, fake.Requests, rc.Requests)
	})
	t.Run("canceled write transaction", func(t *testing.T) {
		rc, cfg, fake := load(t, "canceled write transaction")
		m, p := members(t, cfg), projects(t, cfg)
		err := wattdb.TxWrite(ctx, cfg,
			m.Put(member.Input{OrgID: "O1", MemberID: "M1"}).TxOp(),
			p.Create(project.Input{OrgID: "O1", ProjectID: "P1"}).TxOp(wattdb.TxReturnOld()),
		).Exec()
		vectest.CheckRequests(t, fake.Requests, rc.Requests, "ClientRequestToken")
		var tce *wattdb.TxCanceledError
		if !errors.As(err, &tce) || !errors.Is(err, wattdb.ErrTxCanceled) {
			t.Fatalf("err = %v, want a TxCanceledError", err)
		}
		if len(tce.Reasons) != 2 || tce.Reasons[0].Rejected || tce.Reasons[0].Code != "None" ||
			!tce.Reasons[1].Rejected || tce.Reasons[1].Code != "ConditionalCheckFailed" {
			t.Fatalf("reasons = %+v", tce.Reasons)
		}
		old, ok := tce.Reasons[1].Item.(project.Project)
		want := project.Project{OrgID: "O1", ProjectID: "P1", Name: ptr("Apollo"), Status: project.StatusActive, Owner: ptr("M1"), Due: ptr("2026-03-01")}
		if !ok || !reflect.DeepEqual(old, want) {
			t.Errorf("old item = %#v, want %#v", tce.Reasons[1].Item, want)
		}
	})
	t.Run("get transaction", func(t *testing.T) {
		rc, cfg, fake := load(t, "get transaction")
		m, p := members(t, cfg), projects(t, cfg)
		var a wattdb.TxItem[member.Member]
		var b wattdb.TxItem[project.Project]
		err := wattdb.TxGet(ctx, cfg,
			m.Get(member.Key{OrgID: "O1", MemberID: "M1"}).TxGet(&a),
			p.Get(project.Key{OrgID: "O1", ProjectID: "P9"}).TxGet(&b),
		).Exec()
		if err != nil {
			t.Fatal(err)
		}
		vectest.CheckRequests(t, fake.Requests, rc.Requests)
		want := member.Member{OrgID: "O1", MemberID: "M1", Name: ptr("Ada"), Status: member.StatusActive, Visits: ptr(3)}
		if !a.Found || !reflect.DeepEqual(a.Item, want) || b.Found {
			t.Errorf("got %+v, %+v", a, b)
		}
	})
}

func TestBatches(t *testing.T) {
	keys := func(ids ...string) []member.Key {
		var out []member.Key
		for _, id := range ids {
			out = append(out, member.Key{OrgID: "O1", MemberID: id})
		}
		return out
	}
	t.Run("batch get", func(t *testing.T) {
		rc, cfg, fake := load(t, "batch get")
		found, unprocessed, err := members(t, cfg).BatchGet(keys("M1", "M2", "M3")...).Exec(ctx)
		if err != nil {
			t.Fatal(err)
		}
		vectest.CheckRequests(t, fake.Requests, rc.Requests)
		want := list(t, result(t, rc)["data"], member.Decode)
		if !reflect.DeepEqual(found, want) {
			t.Errorf("found = %+v, want %+v", found, want)
		}
		if !reflect.DeepEqual(unprocessed, keys("M3")) {
			t.Errorf("unprocessed = %+v", unprocessed)
		}
	})
	t.Run("batch get preserving order", func(t *testing.T) {
		rc, cfg, fake := load(t, "batch get preserving order")
		found, unprocessed, err := members(t, cfg).BatchGet(keys("M1", "M3", "M2")...).PreserveOrder().Exec(ctx)
		if err != nil {
			t.Fatal(err)
		}
		vectest.CheckRequests(t, fake.Requests, rc.Requests)
		if len(found) != 3 || found[0] == nil || found[0].MemberID != "M1" || found[1] != nil || found[2] == nil || found[2].MemberID != "M2" {
			t.Errorf("found = %+v", found)
		}
		if len(unprocessed) != 0 {
			t.Errorf("unprocessed = %+v", unprocessed)
		}
	})
	t.Run("batch get of 101 keys", func(t *testing.T) {
		rc, cfg, _ := load(t, "batch get of 101 keys")
		var ks []member.Key
		for i := range 101 {
			ks = append(ks, member.Key{OrgID: "O1", MemberID: "M" + itoa(i)})
		}
		in, err := members(t, cfg).BatchGet(ks...).Params()
		if err != nil {
			t.Fatal(err)
		}
		if len(in) != len(rc.Requests) || len(in[0].RequestItems["table"].Keys) != 100 || len(in[1].RequestItems["table"].Keys) != 1 {
			t.Errorf("chunks: %d", len(in))
		}
	})
	t.Run("batch put", func(t *testing.T) {
		rc, cfg, fake := load(t, "batch put")
		ins := []member.Input{{OrgID: "O1", MemberID: "M1", Name: ptr("Ada")}, {OrgID: "O1", MemberID: "M2"}}
		unprocessed, err := members(t, cfg).BatchPut(ins...).Exec(ctx)
		if err != nil {
			t.Fatal(err)
		}
		vectest.CheckRequests(t, fake.Requests, rc.Requests)
		if !reflect.DeepEqual(unprocessed, ins[1:]) {
			t.Errorf("unprocessed = %+v", unprocessed)
		}
	})
	t.Run("batch put of 26 items", func(t *testing.T) {
		rc, cfg, _ := load(t, "batch put of 26 items")
		var ins []member.Input
		for i := range 26 {
			ins = append(ins, member.Input{OrgID: "O1", MemberID: "M" + itoa(i)})
		}
		in, err := members(t, cfg).BatchPut(ins...).Params()
		if err != nil {
			t.Fatal(err)
		}
		if len(in) != len(rc.Requests) || len(in[0].RequestItems["table"]) != 25 || len(in[1].RequestItems["table"]) != 1 {
			t.Errorf("chunks: %d", len(in))
		}
	})
	t.Run("batch delete", func(t *testing.T) {
		rc, cfg, fake := load(t, "batch delete")
		unprocessed, err := members(t, cfg).BatchDelete(keys("M1", "M2")...).Exec(ctx)
		if err != nil {
			t.Fatal(err)
		}
		vectest.CheckRequests(t, fake.Requests, rc.Requests)
		if !reflect.DeepEqual(unprocessed, keys("M2")) {
			t.Errorf("unprocessed = %+v", unprocessed)
		}
	})
	t.Run("batch put with an invalid item", func(t *testing.T) {
		_, cfg, fake := load(t, "batch put with an invalid item")
		_, err := members(t, cfg).BatchPut(member.Input{OrgID: "O1", MemberID: "M1", Status: ptr(member.StatusEnum("gone"))}).Exec(ctx)
		if !errors.Is(err, wattdb.ErrValidation) || len(fake.Requests) != 0 {
			t.Errorf("err = %v, %d requests", err, len(fake.Requests))
		}
	})
}

func TestCompositeIndex(t *testing.T) {
	for _, tt := range []struct {
		name string
		run  func(p *project.Client) ([]project.Project, wattdb.Cursor, error)
	}{
		{"composite index query", func(p *project.Client) ([]project.Project, wattdb.Cursor, error) {
			return p.ByOwner("O1", "M1").Exec(ctx)
		}},
		{"composite index query with sk", func(p *project.Client) ([]project.Project, wattdb.Cursor, error) {
			return p.ByOwner("O1", "M1").Due().Eq("2026-03-01").Exec(ctx)
		}},
		{"composite index between", func(p *project.Client) ([]project.Project, wattdb.Cursor, error) {
			return p.ByOwner("O1", "M1").Due().Between("2026-01-01", "2026-06-01").Exec(ctx)
		}},
		{"composite index gt", func(p *project.Client) ([]project.Project, wattdb.Cursor, error) {
			return p.ByOwner("O1", "M1").Due().Gt("2026-01-01").Exec(ctx)
		}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			rc, cfg, fake := load(t, tt.name)
			if _, _, err := tt.run(projects(t, cfg)); err != nil {
				t.Fatal(err)
			}
			vectest.CheckRequests(t, fake.Requests, rc.Requests)
		})
	}
}

func TestFind(t *testing.T) {
	for _, tt := range []struct {
		name string
		run  func(cfg *wattdb.Config) error
	}{
		{"find by primary key", func(cfg *wattdb.Config) error {
			_, _, err := members(t, cfg).Find(member.Filter{OrgID: ptr("O1"), MemberID: ptr("M1")}).Exec(ctx)
			return err
		}},
		{"find by pk only", func(cfg *wattdb.Config) error {
			_, _, err := members(t, cfg).Find(member.Filter{OrgID: ptr("O1")}).Exec(ctx)
			return err
		}},
		{"find with no index", func(cfg *wattdb.Config) error {
			_, _, err := members(t, cfg).Find(member.Filter{Visits: ptr(3)}).Exec(ctx)
			return err
		}},
		{"match", func(cfg *wattdb.Config) error {
			_, _, err := members(t, cfg).Match(member.Filter{OrgID: ptr("O1"), Name: ptr("Ada"), Visits: ptr(3)}).Exec(ctx)
			return err
		}},
		{"match with no index", func(cfg *wattdb.Config) error {
			_, _, err := members(t, cfg).Match(member.Filter{Name: ptr("Ada")}).Exec(ctx)
			return err
		}},
		{"find by a gsi", func(cfg *wattdb.Config) error {
			_, _, err := projects(t, cfg).Find(project.Filter{OrgID: ptr("O1"), Name: ptr("Apollo")}).Exec(ctx)
			return err
		}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			rc, cfg, fake := load(t, tt.name)
			if err := tt.run(cfg); err != nil {
				t.Fatal(err)
			}
			vectest.CheckRequests(t, fake.Requests, rc.Requests)
		})
	}
}

func TestConversions(t *testing.T) {
	_, cfg, _ := load(t, "composite to keys")
	conv := members(t, cfg).Conv()
	keys, err := conv.KeysFromComposite(member.Filter{OrgID: ptr("O1"), MemberID: ptr("M1")}, wattdb.StrictNone)
	if err != nil {
		t.Fatal(err)
	}
	if s := keys["gsi2sk"].(*types.AttributeValueMemberS).Value; s != "$board#status_" {
		t.Errorf("gsi2sk = %q", s)
	}
	cur, err := conv.CursorFromKeys(map[string]types.AttributeValue{
		"pk": &types.AttributeValueMemberS{Value: "$taskapp#orgid_o1"},
		"sk": &types.AttributeValueMemberS{Value: "$member_1#memberid_m1"},
	})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := conv.CompositeFromCursor(cur); err == nil {
		t.Error("CompositeFromCursor accepted keys missing gsi1, as ElectroDB doesn't")
	}
	board := conv.Board()
	comp, err := board.CompositeFromKeys(map[string]types.AttributeValue{
		"gsi2pk": &types.AttributeValueMemberS{Value: "$taskapp#orgid_o1"},
		"gsi2sk": &types.AttributeValueMemberS{Value: "$board#status_away#memberid_m2#member_1"},
		"pk":     &types.AttributeValueMemberS{Value: "$taskapp#orgid_o1"},
		"sk":     &types.AttributeValueMemberS{Value: "$member_1#memberid_m2"},
	})
	if err != nil {
		t.Fatal(err)
	}
	want := member.Partial{OrgID: ptr("o1"), MemberID: ptr("m2"), Status: ptr(member.StatusAway)}
	if !reflect.DeepEqual(comp, want) {
		t.Errorf("composite = %+v", comp)
	}
	if _, err := board.KeysFromComposite(member.Filter{OrgID: ptr("O1"), Status: ptr(member.StatusAway)}, wattdb.StrictAll); err == nil {
		t.Error("strict all accepted a partial composite")
	}
}

func TestParse(t *testing.T) {
	_, cfg, _ := load(t, "parse")
	m := members(t, cfg)
	s := func(v string) types.AttributeValue { return &types.AttributeValueMemberS{Value: v} }
	item := map[string]types.AttributeValue{
		"orgId": s("O1"), "memberId": s("M1"), "name": s("Ada"), "status": s("active"),
		"pk": s("$taskapp#orgid_o1"), "sk": s("$member_1#memberid_m1"),
		"__edb_e__": s("other"), "__edb_v__": s("1"),
	}
	got, err := m.Parse(item)
	if err != nil {
		t.Fatal(err)
	}
	if got.MemberID != "M1" {
		t.Errorf("got %+v", got)
	}
	item["pk"] = s("x")
	if _, err := m.Parse(item); !errors.Is(err, wattdb.ErrNotFound) {
		t.Errorf("err = %v, want ErrNotFound", err)
	}
}

func itoa(i int) string {
	if i < 10 {
		return string(rune('0' + i))
	}
	return itoa(i/10) + string(rune('0'+i%10))
}
