// Package schema defines the taskapp entities. wattdb generates typed
// clients for them into ../db.
package schema

import (
	"github.com/outboundani/wattdb"
	"github.com/outboundani/wattdb/attr"
	"github.com/outboundani/wattdb/index"
)

// TaskApp is the service both entities belong to.
var TaskApp = wattdb.Service("taskapp")

// Project is a project in an organization.
type Project struct{}

func (Project) Model() wattdb.Model {
	return wattdb.Model{Entity: "project", Version: "1", Service: TaskApp}
}

func (Project) Attributes() []wattdb.Attribute {
	return []wattdb.Attribute{
		attr.String("OrgID").Field("orgId").Required(),
		attr.String("ProjectID").Field("projectId").Required(),
		attr.String("Name").Field("name").Required(),
		attr.Time("CreatedAt").Field("createdAt").Required().ReadOnly(),
	}
}

func (Project) Indexes() []wattdb.Index {
	return []wattdb.Index{
		index.Primary("primary").PKField("pk").PK("OrgID").SKField("sk").SK("ProjectID"),
		index.GSI("workspace", "gsi1").Collection("workspace").
			PKField("gsi1pk").PK("OrgID", "ProjectID").SKField("gsi1sk").SK(),
	}
}

// Task is a task in a project.
type Task struct{}

func (Task) Model() wattdb.Model {
	return wattdb.Model{Entity: "task", Version: "1", Service: TaskApp}
}

func (Task) Attributes() []wattdb.Attribute {
	return []wattdb.Attribute{
		attr.String("OrgID").Field("orgId").Required(),
		attr.String("ProjectID").Field("projectId").Required(),
		attr.String("TaskID").Field("taskId").Required(),
		attr.String("Title").Field("title").Required(),
		attr.Enum("Status", "open", "doing", "done").Field("status").Required().Default("open"),
		attr.Int("Points").Field("points"),
		attr.Set[string]("Tags").Field("tags"),
		attr.String("Assignee").Field("assignee"),
	}
}

func (Task) Indexes() []wattdb.Index {
	return []wattdb.Index{
		index.Primary("primary").PKField("pk").PK("OrgID", "ProjectID").SKField("sk").SK("TaskID"),
		index.GSI("workspace", "gsi1").Collection("workspace").
			PKField("gsi1pk").PK("OrgID", "ProjectID").SKField("gsi1sk").SK("TaskID"),
		index.GSI("byAssignee", "gsi2").
			PKField("gsi2pk").PK("Assignee").SKField("gsi2sk").SK("Status", "TaskID"),
	}
}
