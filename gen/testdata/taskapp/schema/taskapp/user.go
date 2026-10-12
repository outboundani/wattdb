package taskapp

import (
	"time"

	ttypes "example.com/taskapp/types"
	"github.com/outboundani/wattdb"
	"github.com/outboundani/wattdb/attr"
	"github.com/outboundani/wattdb/codec"
	"github.com/outboundani/wattdb/index"
)

// User exercises the remaining constructors and index options.
type User struct{}

func (User) Model() wattdb.Model {
	return wattdb.Model{Entity: "user", Version: "1", Service: TaskApp}
}

func (User) Attributes() []wattdb.Attribute {
	return []wattdb.Attribute{
		attr.String("OrgID").Field("orgId").Required(),
		attr.String("UserID").Field("userId").Required(),
		attr.String("Email").Field("email").Required(),
		attr.Enum("Role", "admin", "member", "guest").Field("role").Required().Default("member"),
		attr.Int64("LoginCount").Field("loginCount"),
		attr.Float64("Score").Field("score"),
		attr.Bool("Active").Field("active").Default(true),
		attr.Time("CreatedAt").Field("createdAt").Codec(codec.JSISO).DefaultFunc(func() time.Time { return time.Unix(0, 0) }),
		attr.Duration("Timeout").Field("timeout"),
		attr.UUID("Token").Field("token"),
		attr.Set[int]("Lucky").Field("lucky"),
		attr.List("Links", attr.String("")).Field("links"),
		attr.Map("Profile",
			attr.String("DisplayName").Field("displayName"),
			attr.Enum("Pronoun", "they", "she", "he").Field("pronoun"),
			attr.Map("Address", attr.String("City").Field("city")).Field("address"),
		).Field("profile"),
		attr.AnyOf[ttypes.Settings]("Prefs").Field("prefs"),
		attr.Custom[ttypes.Cents]("Balance", ttypes.CentsCodec{}).Field("balance"),
	}
}

func (User) Indexes() []wattdb.Index {
	return []wattdb.Index{
		index.Primary("primary").PKField("pk").PK("OrgID").SKField("sk").SK("UserID"),
		index.GSI("byEmail", "gsi1").PKField("gsi1pk").PK("Email").SKField("gsi1sk").SK(),
		index.GSI("byRole", "gsi2").PKField("gsi2pk").PK("OrgID").
			SKField("gsi2sk").SK("Role", "UserID").Collection("members"),
		index.GSI("byTeam", "gsi3").Type(index.Clustered).
			PKField("gsi3pk").PKTemplate("team#${OrgID}").PKCasing(index.None).
			SKField("gsi3sk").SK("UserID"),
	}
}
