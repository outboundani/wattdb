package taskapp

import (
	"errors"
	"strings"
	"unicode/utf16"

	"example.com/taskapp/db/task"
	"github.com/outboundani/wattdb"
	"github.com/outboundani/wattdb/attr"
	"github.com/outboundani/wattdb/index"
)

// Task mirrors the "callbacks" vector's entity.
type Task struct{}

func (Task) Model() wattdb.Model {
	return wattdb.Model{Entity: "task", Version: "1", Service: TaskApp}
}

func (Task) Attributes() []wattdb.Attribute {
	return []wattdb.Attribute{
		attr.String("TaskID").Field("taskId").Required(),
		attr.String("Title").Field("title").Required().Validate(func(v *string) error {
			if v != nil && len(utf16.Encode([]rune(*v))) < 3 {
				return errors.New("Invalid value provided")
			}
			return nil
		}),
		attr.String("Code").Field("code").ValidateRegexp(`^[A-Z]{2}-\d+$`),
		attr.String("Status").Field("status").
			Set(func(v *string, _ task.Partial) *string {
				if v == nil {
					return nil
				}
				s := strings.ToLower(*v)
				return &s
			}).
			Get(func(v *string, _ task.Partial) *string {
				if v == nil {
					return nil
				}
				s := strings.ToUpper(*v)
				return &s
			}),
		attr.String("Search").Field("search").Watch("Title", "Status").
			Set(func(_ *string, t task.Partial) *string {
				s := deref(t.Title) + "|" + deref(t.Status)
				return &s
			}),
		attr.String("Secret").Field("secret").Hidden(),
		attr.String("CreatedBy").Field("createdBy").ReadOnly(),
		attr.String("Assignee").Field("assignee"),
	}
}

func (Task) Indexes() []wattdb.Index {
	return []wattdb.Index{
		index.Primary("primary").PKField("pk").PK("TaskID").SKField("sk").SK(),
		index.GSI("byAssignee", "gsi1").
			Condition(func(t task.Partial) bool { return t.Assignee != nil }).
			PKField("gsi1pk").PK("Assignee").SKField("gsi1sk").SK(),
	}
}
