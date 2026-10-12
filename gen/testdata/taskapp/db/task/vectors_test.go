package task_test

import (
	"encoding/json"
	"testing"

	_ "example.com/taskapp/db/runtime"
	"example.com/taskapp/db/task"
	"example.com/taskapp/internal/vectest"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
)

func TestMatchesElectroDB(t *testing.T) {
	def, err := task.EntityDef()
	if err != nil {
		t.Fatal(err)
	}
	t.Run("put", func(t *testing.T) {
		vectest.CheckPuts(t, "callbacks", def, func(arg json.RawMessage) (map[string]types.AttributeValue, error) {
			var in task.Input
			if err := vectest.Strict(arg, &in); err != nil {
				return nil, err
			}
			return task.EncodeInput(in)
		})
	})
	t.Run("read", func(t *testing.T) {
		vectest.CheckReads(t, "callbacks", def, func(m map[string]types.AttributeValue) (any, error) { return task.Decode(m) })
	})
}
