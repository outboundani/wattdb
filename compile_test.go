package wattdb

import (
	"fmt"
	"path/filepath"
	"reflect"
	"regexp"
	"strings"
	"testing"

	"github.com/outboundani/wattdb/attr"
	"github.com/outboundani/wattdb/index"
	"github.com/outboundani/wattdb/internal/model"
	"github.com/outboundani/wattdb/internal/vectors"
)

// testSchema is a schema built from literals, for porting vector entities.
type testSchema struct {
	model   Model
	attrs   []Attribute
	indexes []Index
}

func (s testSchema) Model() Model            { return s.model }
func (s testSchema) Attributes() []Attribute { return s.attrs }
func (s testSchema) Indexes() []Index        { return s.indexes }

var (
	taskApp      = Service("taskapp")
	taskAppUpper = Service("TaskApp")
	cb           = func() {} // stands in for a callback; Check only needs presence
)

func entity(svc ServiceDef, name string, attrs []Attribute, indexes ...Index) testSchema {
	return testSchema{Model{Entity: name, Version: "1", Service: svc}, attrs, indexes}
}

// ported are the vector entities written in the DSL, keyed by
// "<vector file>/<entity>".
var ported = map[string]Schema{
	"probes/user": entity(taskAppUpper, "User", []Attribute{
		attr.String("ID").Field("id"),
		attr.String("Org").Field("org").Label("O"),
		attr.Float64("N").Field("n").Padding(5, "0"),
		attr.Bool("B").Field("b"),
		attr.Enum("Status", "Active", "Off").Field("status"),
		attr.Set[string]("Tags").Field("tags"),
		attr.String("Nick").Field("nk"),
	},
		index.Primary("rec").PKField("pk").PK("Org").SKField("sk").SK("ID"),
		index.GSI("byN", "gsi1").SubCollections("c1", "c2").PKField("gsi1pk").PK("Org").SKField("gsi1sk").SK("N", "B"),
		index.GSI("cl", "gsi2").Type(index.Clustered).Collection("cc").PKField("gsi2pk").PK("Status").SKField("gsi2sk").SK("ID"),
		index.GSI("nosk", "gsi3").PKField("gsi3pk").PK("ID"),
	),
	"numbers/metric": entity(taskApp, "metric", []Attribute{
		attr.String("Name").Field("name").Required(),
		attr.Float64("Value").Field("value").Required(),
	}, index.Primary("primary").PKField("pk").PK("Name").SKField("sk").SK("Value")),
	"01-attr-pk/team": entity(taskApp, "team", []Attribute{
		attr.String("TeamID").Field("teamId").Required(),
		attr.String("Name").Field("name").Required(),
		attr.Enum("Plan", "free", "pro").Field("plan").Required(),
		attr.Int("Seats").Field("seats"),
	}, index.Primary("primary").PKField("teamId").PK("TeamID")),
	"02-attr-gsi/project": entity(taskApp, "project", []Attribute{
		attr.String("ProjectID").Field("projectId").Required(),
		attr.String("OwnerID").Field("ownerId").Required(),
		attr.String("Title").Field("title").Required(),
	},
		index.Primary("primary").PKField("projectId").PK("ProjectID"),
		index.GSI("byOwner", "ownerId").PKField("ownerId").PK("OwnerID"),
	),
	"03-numeric-gsi/member": entity(taskApp, "member", []Attribute{
		attr.String("MemberID").Field("memberId").Required(),
		attr.Int("TeamNumber").Field("teamNumber").Required(),
		attr.Int("Rank").Field("rank").Required(),
		attr.Int("Level").Field("level"),
	},
		index.Primary("primary").PKField("memberId").PK("MemberID"),
		index.GSI("byTeamNumber", "teamNumber").PKField("teamNumber").PK("TeamNumber"),
		index.GSI("byRank", "gsi1").PKField("gsi1pk").PK("Rank").PKCast(index.CastNumber),
		index.GSI("byLevelString", "gsi2").PKField("gsi2pk").PK("Level").PKCast(index.CastString),
	),
	"04-numeric-pk/counter": entity(taskApp, "counter", []Attribute{
		attr.Int("CounterID").Field("counterId").Required(),
		attr.Any("Payload").Field("payload"),
	}, index.Primary("primary").PKField("counterId").PK("CounterID")),
	"05-template-none/attempt": entity(taskApp, "attempt", []Attribute{
		attr.String("ProjectID").Field("projectId").Required(),
		attr.String("TaskID").Field("taskId").Required(),
		attr.String("UserID").Field("userId").Required(),
		attr.Int("Count").Field("count"),
		attr.Set[string]("SessionIDs").Field("sessionIds"),
		attr.String("CreatedAt").Field("createdAt").Required().DefaultFunc(cb),
	}, index.Primary("primary").PKField("pk").PK("ProjectID", "TaskID", "UserID").
		PKTemplate("attempt#${ProjectID}#${TaskID}#${UserID}").PKCasing(index.None)),
	"06-template-lower/label": entity(taskApp, "label", []Attribute{
		attr.String("Name").Field("name").Required(),
		attr.Any("Colors").Field("colors").Required(),
	}, index.Primary("primary").PKField("identifier").PK("Name").PKTemplate("label#${Name}")),
	"07-attr-pk-sk/event": entity(taskApp, "event", []Attribute{
		attr.String("TaskID").Field("taskId").Required(),
		attr.Int("Seq").Field("seq").Required(),
		attr.List("Changes", attr.Map("Change",
			attr.String("Field").Field("field").Required(),
			attr.String("From").Field("from"),
			attr.String("To").Field("to"),
		)).Field("changes"),
		attr.Bool("Visible").Field("visible").Required().Default(true),
	}, index.Primary("primary").PKField("taskId").PK("TaskID").SKField("seq").SK("Seq")),
	"08-standard/user": entity(taskApp, "user", []Attribute{
		attr.String("OrgID").Field("orgId").Required(),
		attr.String("UserID").Field("userId").Required(),
		attr.String("Email").Field("email").Required(),
		attr.Enum("Role", "admin", "member", "guest").Field("role").Required().Default("member"),
		attr.Enum("Status", "active", "disabled").Field("status").Default("active"),
		attr.Map("Profile",
			attr.String("DisplayName").Field("displayName"),
			attr.List("Links", attr.String("")).Field("links"),
			attr.Map("Address",
				attr.String("City").Field("city"),
				attr.String("Zip").Field("postalCode"),
			).Field("address"),
		).Field("profile"),
	},
		index.Primary("primary").PKField("pk").PK("OrgID").SKField("sk").SK("UserID"),
		index.GSI("byEmail", "gsi1").PKField("gsi1pk").PK("Email").SKField("gsi1sk").SK(),
		index.GSI("byRole", "gsi2").PKField("gsi2pk").PK("OrgID").SKField("gsi2sk").SK("Role", "UserID"),
	),
	"09-deep-updates/settings": entity(taskApp, "settings", []Attribute{
		attr.String("UserID").Field("userId").Required(),
		attr.Map("Prefs",
			attr.Map("Notifications",
				attr.Map("Email",
					attr.Bool("Enabled").Field("enabled"),
					attr.Enum("Frequency", "instant", "daily", "weekly").Field("frequency"),
				).Field("email"),
				attr.Map("Push", attr.Bool("Enabled").Field("enabled")).Field("push"),
			).Field("notifications"),
			attr.String("Theme").Field("theme"),
		).Field("prefs"),
		attr.Int("Visits").Field("visits"),
		attr.List("Recent", attr.String("")).Field("recent"),
		attr.Set[string]("Flags").Field("flags"),
		attr.Any("Meta").Field("meta"),
	}, index.Primary("primary").PKField("pk").PK("UserID").SKField("sk").SK()),
	"callbacks/task": entity(taskApp, "task", []Attribute{
		attr.String("TaskID").Field("taskId").Required(),
		attr.String("Title").Field("title").Required().Validate(cb),
		attr.String("Code").Field("code").ValidateRegexp(`^[A-Z]{2}-\d+$`),
		attr.String("Status").Field("status").Set(cb).Get(cb),
		attr.String("Search").Field("search").Watch("Title", "Status").Set(cb),
		attr.String("Secret").Field("secret").Hidden(),
		attr.String("CreatedBy").Field("createdBy").ReadOnly(),
		attr.String("Assignee").Field("assignee"),
	},
		index.Primary("primary").PKField("pk").PK("TaskID").SKField("sk").SK(),
		index.GSI("byAssignee", "gsi1").Condition(cb).PKField("gsi1pk").PK("Assignee").SKField("gsi1sk").SK(),
	),
	"mutations/item": entity(taskApp, "item", []Attribute{
		attr.String("OrgID").Field("orgId").Required(),
		attr.String("ItemID").Field("itemId").Required(),
		attr.String("Title").Field("title").Required(),
		attr.Enum("Status", "open", "closed").Field("status").Required().Default("open"),
		attr.String("CreatedAt").Field("createdAt"),
		attr.String("OwnerID").Field("ownerId"),
		attr.String("CreatedBy").Field("createdBy").ReadOnly(),
		attr.Int("Count").Field("count"),
		attr.Set[string]("Tags").Field("tags"),
		attr.List("History", attr.String("")).Field("history"),
		attr.Map("Meta", attr.String("A").Field("a"), attr.Int("B").Field("b")).Field("meta"),
		attr.Any("Blob").Field("blob"),
	},
		index.Primary("primary").PKField("pk").PK("OrgID").SKField("sk").SK("ItemID"),
		index.GSI("byStatus", "gsi1").PKField("gsi1pk").PK("OrgID").SKField("gsi1sk").SK("Status", "CreatedAt"),
		index.GSI("byOwner", "gsi2").Condition(cb).PKField("gsi2pk").PK("OwnerID").SKField("gsi2sk").SK(),
	),
	"queries/task": entity(taskApp, "task", []Attribute{
		attr.String("ProjectID").Field("projectId").Required(),
		attr.String("TaskID").Field("taskId").Required(),
		attr.String("Title").Field("title"),
		attr.Enum("Status", "open", "done").Field("status").Required(),
		attr.Int("Priority").Field("priority").Padding(5, "0"),
		attr.String("CreatedAt").Field("createdAt"),
		attr.String("Assignee").Field("assignee"),
		attr.String("Secret").Field("secret").Hidden(),
	},
		index.Primary("primary").PKField("pk").PK("ProjectID").SKField("sk").SK("TaskID"),
		index.GSI("byStatus", "gsi1").PKField("gsi1pk").PK("ProjectID").SKField("gsi1sk").SK("Status", "Priority", "CreatedAt"),
		index.GSI("byAssignee", "gsi2").Projection(index.KeysOnly).PKField("gsi2pk").PK("Assignee").SKField("gsi2sk").SK("TaskID"),
		index.GSI("byStatusCluster", "gsi3").Type(index.Clustered).PKField("gsi3pk").PK("Status").SKField("gsi3sk").SK("ProjectID", "TaskID"),
		index.GSI("byTitle", "gsi4").Projection(index.Include("Title")).PKField("gsi4pk").PK("ProjectID").SKField("gsi4sk").SK("Title"),
	),
	"pipeline/doc": entity(taskApp, "doc", []Attribute{
		attr.String("DocID").Field("docId").Required(),
		attr.String("Code").Field("code").Set(cb).ValidateRegexp(`^[A-Z]+$`),
		attr.String("Title").Field("t").Get(cb),
		attr.Int("Count").Field("count"),
		attr.Bool("Done").Field("done"),
		attr.Enum("Kind", "note", "task").Field("kind").Default("note"),
		attr.String("Status").Field("status").Required().Default("open"),
		attr.Set[string]("Tags").Field("tags"),
		attr.Set[float64]("Scores").Field("scores"),
		attr.Map("Meta",
			attr.String("Source").Field("source").Default("web"),
			attr.Int("Rank").Field("r"),
			attr.Map("Inner", attr.String("Deep").Field("d")).Field("inner"),
		).Field("meta"),
		attr.Map("Opts", attr.String("A").Field("a")).Field("opts"),
		attr.List("Steps", attr.Map("Step",
			attr.String("Name").Field("n").Required(),
			attr.Bool("OK").Field("ok"),
		)).Field("steps"),
		attr.List("Notes", attr.String("").Validate(cb)).Field("notes"),
		attr.Any("Blob").Field("blob"),
		attr.String("Summary").Field("summary").WatchAll().Set(cb).Get(cb),
		attr.String("Secret").Field("secret").Hidden(),
	}, index.Primary("primary").PKField("pk").PK("DocID").SKField("sk").SK()),
}

// TestCompileMatchesVectorModels checks that each DSL port compiles to the
// same model as the ElectroDB schema recorded in its vector.
func TestCompileMatchesVectorModels(t *testing.T) {
	all, err := vectors.LoadDir(vectors.Dir())
	if err != nil {
		t.Fatal(err)
	}
	seen := 0
	for _, v := range all {
		file := strings.TrimSuffix(filepath.Base(v.File), ".json")
		for _, e := range v.Entities {
			key := file + "/" + e.Name
			t.Run(key, func(t *testing.T) {
				s, ok := ported[key]
				if !ok {
					t.Fatalf("no DSL port for %s", key)
				}
				want, err := e.Model()
				if err != nil {
					t.Fatal(err)
				}
				def, err := definition(s, nil, false)
				if err != nil {
					t.Fatal(err)
				}
				got, err := model.New(def)
				if err != nil {
					t.Fatal(err)
				}
				if g, w := dump(got), dump(want); g != w {
					t.Errorf("models differ\n got: %s\nwant: %s", g, w)
				}
				seen++
			})
		}
	}
	if seen != len(ported) {
		t.Errorf("checked %d entities, ported %d", seen, len(ported))
	}
}

// dump renders a model's exported fields, with functions shown as present
// or absent. Attribute names and paths are skipped: in wattdb the
// ElectroDB name is always the field (D12), while a JS model may differ.
func dump(v any) string {
	var b strings.Builder
	write(&b, reflect.ValueOf(v))
	return b.String()
}

var regexpType = reflect.TypeOf(&regexp.Regexp{})

func write(b *strings.Builder, v reflect.Value) {
	switch v.Kind() {
	case reflect.Func:
		if v.IsNil() {
			b.WriteString("nofunc")
		} else {
			b.WriteString("func")
		}
	case reflect.Pointer:
		if v.IsNil() {
			b.WriteString("nil")
			return
		}
		if v.Type() == regexpType {
			fmt.Fprintf(b, "/%s/", v.Interface())
			return
		}
		write(b, v.Elem())
	case reflect.Interface:
		if v.IsNil() {
			b.WriteString("nil")
			return
		}
		write(b, v.Elem())
	case reflect.Struct:
		b.WriteString("{")
		for i := 0; i < v.NumField(); i++ {
			f := v.Type().Field(i)
			if !f.IsExported() || (v.Type() == reflect.TypeOf(model.Attribute{}) && (f.Name == "Name" || f.Name == "Path")) {
				continue
			}
			fmt.Fprintf(b, "%s:", f.Name)
			write(b, v.Field(i))
			b.WriteString(" ")
		}
		b.WriteString("}")
	case reflect.Slice:
		if v.Len() == 0 {
			b.WriteString("[]")
			return
		}
		b.WriteString("[")
		for i := 0; i < v.Len(); i++ {
			write(b, v.Index(i))
			b.WriteString(",")
		}
		b.WriteString("]")
	default:
		fmt.Fprintf(b, "%v", v.Interface())
	}
}
