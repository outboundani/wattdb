package schema

import (
	"github.com/outboundani/wattdb"
	"github.com/outboundani/wattdb/attr"
	"github.com/outboundani/wattdb/index"
)

// TaskV2 is version 2 of Task: Priority becomes a required enum and
// Assignee a list of assignees. Its package gets MigrateFrom.
type TaskV2 struct{}

func (TaskV2) Model() wattdb.Model {
	return wattdb.Model{Entity: "task", Version: "2", Service: TaskApp}
}

func (TaskV2) Attributes() []wattdb.Attribute {
	return []wattdb.Attribute{
		attr.String("ProjectID").Field("projectId").Required(),
		attr.String("TaskID").Field("taskId").Required(),
		attr.String("Title").Field("title"),
		attr.Enum("Status", "open", "done").Field("status").Required(),
		attr.Enum("Priority", "low", "high").Field("priority").Required().Default("low"),
		attr.List("Assignees", attr.String("")).Field("assignees"),
	}
}

func (TaskV2) Indexes() []wattdb.Index {
	return []wattdb.Index{
		index.Primary("primary").PKField("pk").PK("ProjectID").SKField("sk").SK("TaskID"),
	}
}
