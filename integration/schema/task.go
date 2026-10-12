package schema

import (
	"github.com/outboundani/wattdb"
	"github.com/outboundani/wattdb/attr"
	"github.com/outboundani/wattdb/index"
)

// Task is the "queries" vector entity.
type Task struct{}

func (Task) Model() wattdb.Model {
	return wattdb.Model{Entity: "task", Version: "1", Service: TaskApp}
}

func (Task) Attributes() []wattdb.Attribute {
	return []wattdb.Attribute{
		attr.String("ProjectID").Field("projectId").Required(),
		attr.String("TaskID").Field("taskId").Required(),
		attr.String("Title").Field("title"),
		attr.Enum("Status", "open", "done").Field("status").Required(),
		attr.Int("Priority").Field("priority").Padding(5, "0"),
		attr.String("CreatedAt").Field("createdAt"),
		attr.String("Assignee").Field("assignee"),
		attr.String("Secret").Field("secret").Hidden(),
	}
}

func (Task) Indexes() []wattdb.Index {
	return []wattdb.Index{
		index.Primary("primary").PKField("pk").PK("ProjectID").SKField("sk").SK("TaskID"),
		index.GSI("byStatus", "gsi1").PKField("gsi1pk").PK("ProjectID").SKField("gsi1sk").SK("Status", "Priority", "CreatedAt"),
		index.GSI("byAssignee", "gsi2").Projection(index.KeysOnly).PKField("gsi2pk").PK("Assignee").SKField("gsi2sk").SK("TaskID"),
		index.GSI("byStatusCluster", "gsi3").Type(index.Clustered).PKField("gsi3pk").PK("Status").SKField("gsi3sk").SK("ProjectID", "TaskID"),
		index.GSI("byTitle", "gsi4").Projection(index.Include("Title")).PKField("gsi4pk").PK("ProjectID").SKField("gsi4sk").SK("Title"),
	}
}
