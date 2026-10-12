package wattdb

import (
	"github.com/aws/aws-sdk-go-v2/feature/dynamodb/attributevalue"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
)

func encodeAny(v any) (types.AttributeValue, error) {
	return attributevalue.Marshal(v)
}
