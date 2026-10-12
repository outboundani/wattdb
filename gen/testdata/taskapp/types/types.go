// Package types holds types the schemas use with AnyOf and Custom (they
// can't live in the schema package, decision D62).
package types

import (
	"fmt"

	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb/codec"
)

// Settings is stored as a map via attributevalue.
type Settings struct {
	Theme string `dynamodbav:"theme"`
}

// Cents is money stored as a whole number of cents.
type Cents int64

// CentsCodec stores Cents as a number.
type CentsCodec struct{}

func (CentsCodec) Type() codec.Type { return codec.Number }

func (CentsCodec) Encode(c Cents) (types.AttributeValue, error) {
	return &types.AttributeValueMemberN{Value: fmt.Sprint(int64(c))}, nil
}

func (CentsCodec) Decode(av types.AttributeValue) (Cents, error) {
	n, ok := av.(*types.AttributeValueMemberN)
	if !ok {
		return 0, fmt.Errorf("cents: want a number")
	}
	var c int64
	_, err := fmt.Sscan(n.Value, &c)
	return Cents(c), err
}
