//go:build integration

package integration

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"reflect"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb"
	"github.com/outboundani/wattdb/integration/db/item"
	_ "github.com/outboundani/wattdb/integration/db/runtime"
	"github.com/outboundani/wattdb/internal/vectors"
	"github.com/testcontainers/testcontainers-go"
	"github.com/testcontainers/testcontainers-go/wait"
)

// image is the pinned DynamoDB Local image (decision D63).
const image = "amazon/dynamodb-local:3.3.1"

var (
	ddb    *dynamodb.Client
	tables atomic.Int64
)

func TestMain(m *testing.M) {
	ctx := context.Background()
	c, err := testcontainers.GenericContainer(ctx, testcontainers.GenericContainerRequest{
		ContainerRequest: testcontainers.ContainerRequest{
			Image:        image,
			Cmd:          []string{"-jar", "DynamoDBLocal.jar", "-inMemory", "-sharedDb"},
			ExposedPorts: []string{"8000/tcp"},
			WaitingFor:   wait.ForListeningPort("8000/tcp"),
		},
		Started: true,
	})
	if err != nil {
		fmt.Fprintln(os.Stderr, "starting DynamoDB Local:", err)
		os.Exit(1)
	}
	endpoint, err := c.PortEndpoint(ctx, "8000/tcp", "http")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	ddb = dynamodb.New(dynamodb.Options{
		Region:       "us-east-1",
		BaseEndpoint: aws.String(endpoint),
		Credentials: aws.CredentialsProviderFunc(func(context.Context) (aws.Credentials, error) {
			return aws.Credentials{AccessKeyID: "local", SecretAccessKey: "local"}, nil
		}),
	})
	code := m.Run()
	_ = testcontainers.TerminateContainer(c)
	os.Exit(code)
}

// newTable creates a fresh table with the item entity's key layout.
func newTable(t *testing.T) string {
	t.Helper()
	name := fmt.Sprintf("table%d", tables.Add(1))
	str := func(n string) types.AttributeDefinition {
		return types.AttributeDefinition{AttributeName: aws.String(n), AttributeType: types.ScalarAttributeTypeS}
	}
	gsi := func(name, pk, sk string) types.GlobalSecondaryIndex {
		return types.GlobalSecondaryIndex{
			IndexName: aws.String(name),
			KeySchema: []types.KeySchemaElement{
				{AttributeName: aws.String(pk), KeyType: types.KeyTypeHash},
				{AttributeName: aws.String(sk), KeyType: types.KeyTypeRange},
			},
			Projection: &types.Projection{ProjectionType: types.ProjectionTypeAll},
		}
	}
	_, err := ddb.CreateTable(context.Background(), &dynamodb.CreateTableInput{
		TableName:   aws.String(name),
		BillingMode: types.BillingModePayPerRequest,
		AttributeDefinitions: []types.AttributeDefinition{
			str("pk"), str("sk"), str("gsi1pk"), str("gsi1sk"), str("gsi2pk"), str("gsi2sk"),
		},
		KeySchema: []types.KeySchemaElement{
			{AttributeName: aws.String("pk"), KeyType: types.KeyTypeHash},
			{AttributeName: aws.String("sk"), KeyType: types.KeyTypeRange},
		},
		GlobalSecondaryIndexes: []types.GlobalSecondaryIndex{gsi("gsi1", "gsi1pk", "gsi1sk"), gsi("gsi2", "gsi2pk", "gsi2sk")},
	})
	if err != nil {
		t.Fatal(err)
	}
	return name
}

func client(t *testing.T, table string) *item.Client {
	t.Helper()
	c, err := item.New(wattdb.NewConfig(ddb), table)
	if err != nil {
		t.Fatal(err)
	}
	return c
}

func raw(t *testing.T, table string, orgID, itemID string) map[string]types.AttributeValue {
	t.Helper()
	out, err := ddb.GetItem(context.Background(), &dynamodb.GetItemInput{
		TableName: aws.String(table),
		Key: map[string]types.AttributeValue{
			"pk": &types.AttributeValueMemberS{Value: "$taskapp#orgid_" + strings.ToLower(orgID)},
			"sk": &types.AttributeValueMemberS{Value: "$item_1#itemid_" + strings.ToLower(itemID)},
		},
		ConsistentRead: aws.Bool(true),
	})
	if err != nil {
		t.Fatal(err)
	}
	return out.Item
}

func str(s string) *string { return &s }

func base() item.Input {
	return item.Input{OrgID: "O1", ItemID: "I1", Title: "T", CreatedAt: str("2026-01-01")}
}

// TestPutsMatchElectroDB writes each recorded put/create input through the
// generated client and compares the stored item with ElectroDB's.
func TestPutsMatchElectroDB(t *testing.T) {
	v, err := vectors.Load(vectors.Dir() + "/mutations.json")
	if err != nil {
		t.Fatal(err)
	}
	checked := 0
	for _, c := range v.Cases {
		method := c.Chain[0].Method
		if (method != "put" && method != "create") || c.Params == nil || len(c.Chain) != 1 {
			continue
		}
		t.Run(c.Name, func(t *testing.T) {
			var in item.Input
			dec := json.NewDecoder(bytes.NewReader(c.Chain[0].Args[0]))
			dec.DisallowUnknownFields()
			if err := dec.Decode(&in); err != nil {
				t.Fatal(err)
			}
			table := newTable(t)
			cl := client(t, table)
			var err error
			if method == "create" {
				_, err = cl.Create(in).Exec(context.Background())
			} else {
				_, err = cl.Put(in).Exec(context.Background())
			}
			if err != nil {
				t.Fatal(err)
			}
			var req struct {
				Item json.RawMessage `json:"Item"`
			}
			json.Unmarshal(c.Params[0].Request, &req)
			want, err := vectors.AttributeMap(req.Item)
			if err != nil {
				t.Fatal(err)
			}
			if got := raw(t, table, in.OrgID, in.ItemID); !reflect.DeepEqual(got, want) {
				t.Errorf("stored item differs\n got: %v\nwant: %v", got, want)
			}
			checked++
		})
	}
	if checked == 0 {
		t.Fatal("no cases checked")
	}
}

func TestGetAndDelete(t *testing.T) {
	ctx := context.Background()
	cl := client(t, newTable(t))
	k := item.Key{OrgID: "O1", ItemID: "I1"}
	if _, err := cl.Get(k).Exec(ctx); !errors.Is(err, wattdb.ErrNotFound) {
		t.Fatalf("Get on a missing item: %v", err)
	}
	in := base()
	in.Tags = []string{"b", "a", "b"}
	in.Meta = &item.MetaMap{A: str("x")}
	put, err := cl.Put(in).Exec(ctx)
	if err != nil {
		t.Fatal(err)
	}
	got, err := cl.Get(k).Consistent().Exec(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if got.Title != "T" || got.Status != item.StatusOpen || !reflect.DeepEqual(got.Tags, []string{"a", "b"}) && !reflect.DeepEqual(got.Tags, []string{"b", "a"}) {
		t.Errorf("Get = %+v", got)
	}
	if put.Status != item.StatusOpen {
		t.Errorf("Put returned %+v", put)
	}
	if err := cl.Delete(k).Exec(ctx); err != nil {
		t.Fatal(err)
	}
	if _, err := cl.Get(k).Exec(ctx); !errors.Is(err, wattdb.ErrNotFound) {
		t.Fatalf("Get after delete: %v", err)
	}
}

func TestConditionFailures(t *testing.T) {
	ctx := context.Background()
	cl := client(t, newTable(t))
	k := item.Key{OrgID: "O1", ItemID: "I1"}

	if err := cl.Remove(k).Exec(ctx); !errors.Is(err, wattdb.ErrConditionFailed) {
		t.Errorf("Remove on a missing item: %v", err)
	}
	if err := cl.Patch(k).Set(item.Title, "x").Exec(ctx); !errors.Is(err, wattdb.ErrConditionFailed) {
		t.Errorf("Patch on a missing item: %v", err)
	}
	if _, err := cl.Create(base()).Exec(ctx); err != nil {
		t.Fatal(err)
	}
	if _, err := cl.Create(base()).Exec(ctx); !errors.Is(err, wattdb.ErrConditionFailed) {
		t.Errorf("second Create: %v", err)
	}
	o, err := cl.Create(base()).ReturnRejected(wattdb.WithOld()).Exec(ctx)
	if err != nil || !o.Rejected || o.Item == nil || o.Item.Title != "T" {
		t.Errorf("Create with ReturnRejected(WithOld()) = %+v, %v", o, err)
	}
	o, err = cl.Create(base()).ReturnRejected().Exec(ctx)
	if err != nil || !o.Rejected || o.Item != nil {
		t.Errorf("Create with ReturnRejected() = %+v, %v", o, err)
	}
	err = cl.Update(k).Set(item.Title, "y").Where(item.Status.Eq(item.StatusClosed)).Exec(ctx)
	if !errors.Is(err, wattdb.ErrConditionFailed) {
		t.Errorf("Update with a false condition: %v", err)
	}
	err = cl.Update(k).Set(item.Title, "y").Where(wattdb.And(item.Status.Eq(item.StatusOpen), item.Title.BeginsWith("T"))).Exec(ctx)
	if err != nil {
		t.Errorf("Update with a true condition: %v", err)
	}
}

func TestUpdates(t *testing.T) {
	ctx := context.Background()
	table := newTable(t)
	cl := client(t, table)
	k := item.Key{OrgID: "O1", ItemID: "I1"}
	in := base()
	in.Count = func(n int) *int { return &n }(5)
	in.Blob = map[string]any{"x": 1}
	in.History = []string{"h0"}
	in.Meta = &item.MetaMap{A: str("a")}
	if _, err := cl.Put(in).Exec(ctx); err != nil {
		t.Fatal(err)
	}

	err := cl.Update(k).
		Set(item.Title, "New").
		Set(item.MetaA, "b").
		Add(item.Count, 2).
		Add(item.Tags, []string{"t1", "t2"}).
		Append(item.History, []string{"h1"}).
		IfNotExists(item.MetaB, 9).
		Remove(item.Blob).
		Exec(ctx)
	if err != nil {
		t.Fatal(err)
	}
	// DynamoDB rejects overlapping paths in one update, so the list item is
	// set separately.
	if err := cl.Update(k).Set(item.History.At(0), "first").Exec(ctx); err != nil {
		t.Fatal(err)
	}
	got, err := cl.Get(k).Consistent().Exec(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if got.Title != "New" || *got.Count != 7 || got.Meta.A == nil || *got.Meta.A != "b" || *got.Meta.B != 9 ||
		!reflect.DeepEqual(got.History, []string{"first", "h1"}) || got.Blob != nil || len(got.Tags) != 2 {
		t.Errorf("after update: %+v (meta %+v)", got, got.Meta)
	}

	if err := cl.Update(k).Subtract(item.Count, 10).Delete(item.Tags, []string{"t1"}).Exec(ctx); err != nil {
		t.Fatal(err)
	}
	got, _ = cl.Get(k).Consistent().Exec(ctx)
	if *got.Count != -3 || !reflect.DeepEqual(got.Tags, []string{"t2"}) {
		t.Errorf("after subtract/delete: count %d tags %v", *got.Count, got.Tags)
	}

	// Sparse index: setting the owner writes gsi2 keys; removing it removes
	// gsi2pk but, as in ElectroDB, not the composite-less gsi2sk.
	if err := cl.Update(k).Set(item.OwnerID, "U1").Exec(ctx); err != nil {
		t.Fatal(err)
	}
	r := raw(t, table, "O1", "I1")
	if r["gsi2pk"] == nil || r["gsi2sk"] == nil {
		t.Errorf("sparse keys missing after set: %v", r)
	}
	if err := cl.Update(k).Remove(item.OwnerID).Exec(ctx); err != nil {
		t.Fatal(err)
	}
	r = raw(t, table, "O1", "I1")
	if r["gsi2pk"] != nil || r["gsi2sk"] == nil {
		t.Errorf("after removing the owner: gsi2pk %v gsi2sk %v", r["gsi2pk"], r["gsi2sk"])
	}

	// A gsi composite without its sibling fails; Composite supplies it.
	if err := cl.Update(k).Set(item.Status, item.StatusClosed).Exec(ctx); !errors.Is(err, wattdb.ErrIncompleteComposites) {
		t.Errorf("update of one gsi composite: %v", err)
	}
	if err := cl.Update(k).Set(item.Status, item.StatusClosed).Composite(item.CreatedAt, "2026-01-01").Exec(ctx); err != nil {
		t.Fatal(err)
	}
	r = raw(t, table, "O1", "I1")
	if s := r["gsi1sk"].(*types.AttributeValueMemberS).Value; s != "$item_1#status_closed#createdat_2026-01-01" {
		t.Errorf("gsi1sk = %s", s)
	}

	// Validation and readOnly errors come back from Exec.
	if err := cl.Update(k).Set(item.CreatedBy, "x").Exec(ctx); !errors.Is(err, wattdb.ErrValidation) {
		t.Errorf("update of a readOnly attribute: %v", err)
	}
	if err := cl.Update(k).Remove(item.Title).Exec(ctx); !errors.Is(err, wattdb.ErrValidation) {
		t.Errorf("removing a required attribute: %v", err)
	}
}

func TestReturning(t *testing.T) {
	ctx := context.Background()
	cl := client(t, newTable(t))
	k := item.Key{OrgID: "O1", ItemID: "I1"}
	if _, err := cl.Put(base()).Exec(ctx); err != nil {
		t.Fatal(err)
	}
	after, err := cl.Update(k).Set(item.Title, "A").Returning(wattdb.AllNew).Exec(ctx)
	if err != nil || after.Title != "A" {
		t.Errorf("Returning(AllNew) = %+v, %v", after, err)
	}
	old, err := cl.Update(k).Set(item.Title, "B").ReturningUpdated(wattdb.UpdatedOld).Exec(ctx)
	if err != nil || old.Title == nil || *old.Title != "A" || old.Status != nil {
		t.Errorf("ReturningUpdated(UpdatedOld) = %+v, %v", old, err)
	}
	gone, err := cl.Delete(k).Returning(wattdb.AllOld).Exec(ctx)
	if err != nil || gone.Title != "B" {
		t.Errorf("Delete Returning(AllOld) = %+v, %v", gone, err)
	}
	if _, err := cl.Delete(k).Returning(wattdb.AllOld).Exec(ctx); !errors.Is(err, wattdb.ErrNotFound) {
		t.Errorf("Delete Returning(AllOld) on a missing item: %v", err)
	}
}

func TestUpsert(t *testing.T) {
	ctx := context.Background()
	table := newTable(t)
	cl := client(t, table)
	k := item.Key{OrgID: "O1", ItemID: "I1"}
	in := base()
	in.CreatedBy = str("u1")
	if err := cl.Upsert(in).Add(item.Count, 1).Exec(ctx); err != nil {
		t.Fatal(err)
	}
	in.CreatedBy = str("u2")
	in.Title = "T2"
	if err := cl.Upsert(in).Add(item.Count, 1).Exec(ctx); err != nil {
		t.Fatal(err)
	}
	got, err := cl.Get(k).Consistent().Exec(ctx)
	if err != nil {
		t.Fatal(err)
	}
	// createdBy is readOnly: upsert only sets it if missing.
	if got.Title != "T2" || *got.CreatedBy != "u1" || *got.Count != 2 {
		t.Errorf("after upserts: %+v", got)
	}
}

// TestReadsElectroDBItem stores an item exactly as ElectroDB wrote it and
// reads it through the generated client.
func TestReadsElectroDBItem(t *testing.T) {
	v, err := vectors.Load(vectors.Dir() + "/mutations.json")
	if err != nil {
		t.Fatal(err)
	}
	var stored map[string]types.AttributeValue
	for _, c := range v.Cases {
		if c.Name == "create" {
			var req struct {
				Item json.RawMessage `json:"Item"`
			}
			json.Unmarshal(c.Params[0].Request, &req)
			if stored, err = vectors.AttributeMap(req.Item); err != nil {
				t.Fatal(err)
			}
		}
	}
	table := newTable(t)
	if _, err := ddb.PutItem(context.Background(), &dynamodb.PutItemInput{TableName: aws.String(table), Item: stored}); err != nil {
		t.Fatal(err)
	}
	got, err := client(t, table).Get(item.Key{OrgID: "O1", ItemID: "I1"}).Exec(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if got.OrgID != "O1" || got.Title != "T" || got.Status != item.StatusOpen || *got.CreatedAt != "2026-01-01" {
		t.Errorf("read %+v", got)
	}
}
