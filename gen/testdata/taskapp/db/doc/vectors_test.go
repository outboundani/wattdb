package doc_test

import (
	"encoding/json"
	"testing"

	"example.com/taskapp/db/doc"
	_ "example.com/taskapp/db/runtime"
	"example.com/taskapp/internal/vectest"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
)

func TestMatchesElectroDB(t *testing.T) {
	def, err := doc.EntityDef()
	if err != nil {
		t.Fatal(err)
	}
	t.Run("put", func(t *testing.T) {
		vectest.CheckPuts(t, "pipeline", def, func(arg json.RawMessage) (map[string]types.AttributeValue, error) {
			var in doc.Input
			if err := vectest.Strict(arg, &in); err != nil {
				return nil, err
			}
			return doc.EncodeInput(in)
		})
	})
	t.Run("read", func(t *testing.T) {
		vectest.CheckReads(t, "pipeline", def, func(m map[string]types.AttributeValue) (any, error) { return doc.Decode(m) })
	})
}
