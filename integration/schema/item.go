// Package schema holds the integration tests' schemas. Item matches the
// "mutations" vector entity, so items written by wattdb can be compared
// with the items ElectroDB recorded.
package schema

import (
	"github.com/outboundani/wattdb"
	"github.com/outboundani/wattdb/attr"
	"github.com/outboundani/wattdb/index"
	"github.com/outboundani/wattdb/integration/db/item"
)

// TaskApp is the service the integration entities belong to.
var TaskApp = wattdb.Service("taskapp")

// Item is the "mutations" vector entity.
type Item struct{}

func (Item) Model() wattdb.Model {
	return wattdb.Model{Entity: "item", Version: "1", Service: TaskApp}
}

func (Item) Attributes() []wattdb.Attribute {
	return []wattdb.Attribute{
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
	}
}

func (Item) Indexes() []wattdb.Index {
	return []wattdb.Index{
		index.Primary("primary").PKField("pk").PK("OrgID").SKField("sk").SK("ItemID"),
		index.GSI("byStatus", "gsi1").PKField("gsi1pk").PK("OrgID").SKField("gsi1sk").SK("Status", "CreatedAt"),
		index.GSI("byOwner", "gsi2").
			Condition(func(i item.Partial) bool { return i.OwnerID != nil }).
			PKField("gsi2pk").PK("OwnerID").SKField("gsi2sk").SK(),
	}
}
