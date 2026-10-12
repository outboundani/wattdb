package task

import ddbtypes "github.com/aws/aws-sdk-go-v2/service/dynamodb/types"

// Exports for the vector tests.

func EncodeInput(in Input) (map[string]ddbtypes.AttributeValue, error) { return in.encode() }

var EntityDef = entityDef

var Decode = decodeTask
