package schema

import (
	"github.com/outboundani/wattdb"
	"github.com/outboundani/wattdb/attr"
	"github.com/outboundani/wattdb/index"
)

// Member and Project are the "multi" vector entities: batches,
// transactions, collections and a composite index.
type Member struct{}

func (Member) Model() wattdb.Model {
	return wattdb.Model{Entity: "member", Version: "1", Service: TaskApp}
}

func (Member) Attributes() []wattdb.Attribute {
	return []wattdb.Attribute{
		attr.String("OrgID").Field("orgId").Required(),
		attr.String("MemberID").Field("memberId").Required(),
		attr.String("Name").Field("name"),
		attr.Enum("Status", "active", "away").Field("status").Required().Default("active"),
		attr.Int("Visits").Field("visits"),
	}
}

func (Member) Indexes() []wattdb.Index {
	return []wattdb.Index{
		index.Primary("primary").PKField("pk").PK("OrgID").SKField("sk").SK("MemberID"),
		index.GSI("team", "gsi1").Collection("team").PKField("gsi1pk").PK("OrgID").SKField("gsi1sk").SK("MemberID"),
		index.GSI("board", "gsi2").Type(index.Clustered).Collection("board").
			PKField("gsi2pk").PK("OrgID").SKField("gsi2sk").SK("Status", "MemberID"),
		index.GSI("org", "gsi3").SubCollections("org", "orgMembers").
			PKField("gsi3pk").PK("OrgID").SKField("gsi3sk").SK("Name"),
	}
}

type Project struct{}

func (Project) Model() wattdb.Model {
	return wattdb.Model{Entity: "project", Version: "1", Service: TaskApp}
}

func (Project) Attributes() []wattdb.Attribute {
	return []wattdb.Attribute{
		attr.String("OrgID").Field("orgId").Required(),
		attr.String("ProjectID").Field("projectId").Required(),
		attr.String("Name").Field("name"),
		attr.Enum("Status", "active", "away").Field("status").Required().Default("active"),
		attr.String("Owner").Field("owner"),
		attr.String("Due").Field("due"),
	}
}

func (Project) Indexes() []wattdb.Index {
	return []wattdb.Index{
		index.Primary("primary").PKField("pk").PK("OrgID").SKField("sk").SK("ProjectID"),
		index.GSI("team", "gsi1").Collection("team").PKField("gsi1pk").PK("OrgID").SKField("gsi1sk").SK("ProjectID"),
		index.GSI("board", "gsi2").Type(index.Clustered).Collection("board").
			PKField("gsi2pk").PK("OrgID").SKField("gsi2sk").SK("Status", "ProjectID"),
		index.GSI("org", "gsi3").SubCollections("org", "orgProjects").
			PKField("gsi3pk").PK("OrgID").SKField("gsi3sk").SK("Name"),
		index.GSI("byOwner", "gsi4").Type(index.Composite).PK("OrgID", "Owner").SK("Due", "ProjectID"),
	}
}
