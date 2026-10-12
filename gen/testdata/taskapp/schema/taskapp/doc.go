package taskapp

import (
	"errors"
	"fmt"
	"strings"
	"unicode/utf16"

	"example.com/taskapp/db/doc"
	"github.com/outboundani/wattdb"
	"github.com/outboundani/wattdb/attr"
	"github.com/outboundani/wattdb/index"
)

// Doc mirrors the "pipeline" vector's entity, with its registry callbacks
// written in Go.
type Doc struct{}

func (Doc) Model() wattdb.Model {
	return wattdb.Model{Entity: "doc", Version: "1", Service: TaskApp}
}

func (Doc) Attributes() []wattdb.Attribute {
	return []wattdb.Attribute{
		attr.String("DocID").Field("docId").Required(),
		attr.String("Code").Field("code").Set(upper).ValidateRegexp(`^[A-Z]+$`),
		attr.String("Title").Field("t").JSONName("title").Get(func(v *string, _ doc.Partial) *string {
			if v == nil {
				return nil
			}
			s := "got:" + *v
			return &s
		}),
		attr.Int("Count").Field("count"),
		attr.Bool("Done").Field("done"),
		attr.Enum("Kind", "note", "task").Field("kind").Default("note"),
		attr.String("Status").Field("status").Required().Default("open"),
		attr.Set[string]("Tags").Field("tags"),
		attr.Set[float64]("Scores").Field("scores"),
		attr.Map("Meta",
			attr.String("Source").Field("source").Default("web"),
			attr.Int("Rank").Field("r").JSONName("rank"),
			attr.Map("Inner", attr.String("Deep").Field("d").JSONName("deep")).Field("inner"),
		).Field("meta"),
		attr.Map("Opts", attr.String("A").Field("a")).Field("opts"),
		attr.List("Steps", attr.Map("Step",
			attr.String("Name").Field("n").JSONName("name").Required(),
			attr.Bool("OK").Field("ok"),
		)).Field("steps"),
		attr.List("Notes", attr.String("").Validate(func(v *string) error {
			if v != nil && len(utf16.Encode([]rune(*v))) < 2 {
				return errors.New("Invalid value provided")
			}
			return nil
		})).Field("notes"),
		attr.Any("Blob").Field("blob"),
		attr.String("Summary").Field("summary").WatchAll().
			Set(func(_ *string, d doc.Partial) *string {
				s := fmt.Sprintf("%s/%s", deref(d.Kind), deref(d.Status))
				return &s
			}).
			Get(func(v *string, _ doc.Partial) *string {
				if v == nil {
					return nil
				}
				s := strings.ToUpper(*v)
				return &s
			}),
		attr.String("Secret").Field("secret").Hidden(),
	}
}

func (Doc) Indexes() []wattdb.Index {
	return []wattdb.Index{
		index.Primary("primary").PKField("pk").PK("DocID").SKField("sk").SK(),
	}
}

func upper(v *string, _ doc.Partial) *string {
	if v == nil {
		return nil
	}
	s := strings.ToUpper(*v)
	return &s
}
