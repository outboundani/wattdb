package main

import (
	"context"
	"errors"
	"fmt"
	"log"
	"log/slog"
	"os"
	"time"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb"
	"github.com/outboundani/wattdb/examples/taskapp/db/project"
	_ "github.com/outboundani/wattdb/examples/taskapp/db/runtime" // registers the schemas
	"github.com/outboundani/wattdb/examples/taskapp/db/task"
	"github.com/outboundani/wattdb/examples/taskapp/db/taskapp"
)

// Run against DynamoDB Local:
//
//	docker run -p 8000:8000 amazon/dynamodb-local:3.3.1
//	go run ./examples/taskapp
//
// The table needs pk/sk plus gsi1 (gsi1pk/gsi1sk) and gsi2 (gsi2pk/gsi2sk);
// the program creates it when it's missing.
func main() {
	ctx := context.Background()
	endpoint := os.Getenv("DYNAMODB_ENDPOINT")
	if endpoint == "" {
		endpoint = "http://localhost:8000"
	}
	ddb := dynamodb.New(dynamodb.Options{
		Region:       "us-east-1",
		BaseEndpoint: aws.String(endpoint),
		Credentials: aws.CredentialsProviderFunc(func(context.Context) (aws.Credentials, error) {
			return aws.Credentials{AccessKeyID: "local", SecretAccessKey: "local"}, nil
		}),
	})
	if err := run(ctx, ddb, "taskapp"); err != nil {
		log.Fatal(err)
	}
}

func run(ctx context.Context, ddb *dynamodb.Client, table string) error {
	if err := ensureTable(ctx, ddb, table); err != nil {
		return err
	}

	// One Config serves every client; the logger logs each request.
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelDebug}))
	cfg := wattdb.NewConfig(ddb, wattdb.WithLogger(logger))

	projects, err := project.New(cfg, table)
	if err != nil {
		return err
	}
	tasks, err := task.New(cfg, table)
	if err != nil {
		return err
	}

	// Create fails with ErrConditionFailed when the item exists.
	_, err = projects.Create(project.Input{OrgID: "acme", ProjectID: "web", Name: "Website", CreatedAt: time.Now()}).Exec(ctx)
	if err != nil && !errors.Is(err, wattdb.ErrConditionFailed) {
		return err
	}

	// Batch writes chunk and can retry unprocessed items.
	var batch []task.Input
	for i, title := range []string{"Design", "Build", "Ship"} {
		batch = append(batch, task.Input{
			OrgID: "acme", ProjectID: "web", TaskID: fmt.Sprintf("t%d", i+1),
			Title: title, Points: ptr(i + 1), Assignee: ptr("ada"),
		})
	}
	if left, err := tasks.BatchPut(batch...).RetryUnprocessed(wattdb.Backoff(5, 50*time.Millisecond)).Exec(ctx); err != nil || len(left) > 0 {
		return fmt.Errorf("batch put: %v (%d left)", err, len(left))
	}

	// Typed updates and conditions.
	err = tasks.Update(task.Key{OrgID: "acme", ProjectID: "web", TaskID: "t1"}).
		Set(task.Status, task.StatusDoing).
		Add(task.Tags, []string{"ux"}).
		Where(task.Status.Eq(task.StatusOpen)).
		Exec(ctx)
	if err != nil && !errors.Is(err, wattdb.ErrConditionFailed) {
		return err
	}

	// Queries take the partition key composites, then sort-key steps.
	open, _, err := tasks.ByAssignee("ada").Status().Eq(task.StatusOpen).Exec(ctx)
	if err != nil {
		return err
	}
	fmt.Printf("ada has %d open tasks\n", len(open))

	// Transactions are all or nothing.
	err = wattdb.TxWrite(ctx, cfg,
		tasks.Update(task.Key{OrgID: "acme", ProjectID: "web", TaskID: "t2"}).Set(task.Status, task.StatusDone).TxOp(),
		projects.Check(project.Key{OrgID: "acme", ProjectID: "web"}).Where(project.Name.Exists()).TxOp(),
	).Exec()
	var canceled *wattdb.TxCanceledError
	if errors.As(err, &canceled) {
		fmt.Println("transaction canceled:", canceled.Reasons)
	} else if err != nil {
		return err
	}

	// A collection reads a project and its tasks in one query.
	ws, err := taskapp.NewWorkspace(cfg, table)
	if err != nil {
		return err
	}
	r, _, err := ws.Query(taskapp.WorkspaceKey{OrgID: "acme", ProjectID: "web"}).Exec(ctx)
	if err != nil {
		return err
	}
	for _, p := range r.Projects {
		fmt.Printf("project %s (%s)\n", p.Name, p.CreatedAt.Format(time.DateOnly))
	}
	for _, t := range r.Tasks {
		fmt.Printf("  %s %-6s %s\n", t.TaskID, t.Status, t.Title)
	}

	// Iterators read every page.
	total := 0
	for t, err := range tasks.Primary("acme", "web").All(ctx) {
		if err != nil {
			return err
		}
		if t.Points != nil {
			total += *t.Points
		}
	}
	fmt.Println("total points:", total)
	return nil
}

func ensureTable(ctx context.Context, ddb *dynamodb.Client, table string) error {
	_, err := ddb.DescribeTable(ctx, &dynamodb.DescribeTableInput{TableName: aws.String(table)})
	if err == nil {
		return nil
	}
	s := func(name string) types.AttributeDefinition {
		return types.AttributeDefinition{AttributeName: aws.String(name), AttributeType: types.ScalarAttributeTypeS}
	}
	key := func(pk, sk string) []types.KeySchemaElement {
		return []types.KeySchemaElement{
			{AttributeName: aws.String(pk), KeyType: types.KeyTypeHash},
			{AttributeName: aws.String(sk), KeyType: types.KeyTypeRange},
		}
	}
	all := &types.Projection{ProjectionType: types.ProjectionTypeAll}
	_, err = ddb.CreateTable(ctx, &dynamodb.CreateTableInput{
		TableName:            aws.String(table),
		BillingMode:          types.BillingModePayPerRequest,
		AttributeDefinitions: []types.AttributeDefinition{s("pk"), s("sk"), s("gsi1pk"), s("gsi1sk"), s("gsi2pk"), s("gsi2sk")},
		KeySchema:            key("pk", "sk"),
		GlobalSecondaryIndexes: []types.GlobalSecondaryIndex{
			{IndexName: aws.String("gsi1"), KeySchema: key("gsi1pk", "gsi1sk"), Projection: all},
			{IndexName: aws.String("gsi2"), KeySchema: key("gsi2pk", "gsi2sk"), Projection: all},
		},
	})
	return err
}

func ptr[T any](v T) *T { return &v }
