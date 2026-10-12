package wattdb

import (
	"errors"
	"fmt"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
	"github.com/outboundani/wattdb/internal/engine"
)

// sdkParams holds an engine request's fields in SDK types.
type sdkParams struct {
	table      string
	key        map[string]types.AttributeValue
	item       map[string]types.AttributeValue
	update     *string
	condition  *string
	projection *string
	names      map[string]string
	values     map[string]types.AttributeValue
	returnVals types.ReturnValue
	returnCCF  types.ReturnValuesOnConditionCheckFailure
	consistent *bool
}

func toSDK(r *engine.Request) (sdkParams, error) {
	var p sdkParams
	var err error
	for k, v := range r.Params {
		switch k {
		case "TableName":
			p.table = v.(string)
		case "Key":
			p.key, err = engine.ToAttributeMap(v.(map[string]any))
		case "Item":
			p.item, err = engine.ToAttributeMap(v.(map[string]any))
		case "UpdateExpression":
			p.update = aws.String(v.(string))
		case "ConditionExpression":
			p.condition = aws.String(v.(string))
		case "ProjectionExpression":
			p.projection = aws.String(v.(string))
		case "ExpressionAttributeNames":
			p.names = v.(map[string]string)
		case "ExpressionAttributeValues":
			p.values, err = engine.ToAttributeMap(v.(map[string]any))
		case "ReturnValues":
			p.returnVals = types.ReturnValue(v.(string))
		case "ReturnValuesOnConditionCheckFailure":
			p.returnCCF = types.ReturnValuesOnConditionCheckFailure(v.(string))
		case "ConsistentRead":
			p.consistent = aws.Bool(v.(bool))
		default:
			return p, fmt.Errorf("wattdb: unexpected request field %s", k)
		}
		if err != nil {
			return p, fmt.Errorf("wattdb: %s: %w", k, err)
		}
	}
	if len(p.names) == 0 {
		p.names = nil
	}
	if len(p.values) == 0 {
		p.values = nil
	}
	return p, nil
}

func (p sdkParams) get() *dynamodb.GetItemInput {
	return &dynamodb.GetItemInput{
		TableName: aws.String(p.table), Key: p.key, ConsistentRead: p.consistent,
		ProjectionExpression: p.projection, ExpressionAttributeNames: p.names,
	}
}

func (p sdkParams) put() *dynamodb.PutItemInput {
	return &dynamodb.PutItemInput{
		TableName: aws.String(p.table), Item: p.item, ConditionExpression: p.condition,
		ExpressionAttributeNames: p.names, ExpressionAttributeValues: p.values,
		ReturnValues: p.returnVals, ReturnValuesOnConditionCheckFailure: p.returnCCF,
	}
}

func (p sdkParams) delete() *dynamodb.DeleteItemInput {
	return &dynamodb.DeleteItemInput{
		TableName: aws.String(p.table), Key: p.key, ConditionExpression: p.condition,
		ExpressionAttributeNames: p.names, ExpressionAttributeValues: p.values,
		ReturnValues: p.returnVals, ReturnValuesOnConditionCheckFailure: p.returnCCF,
	}
}

func (p sdkParams) updateItem() *dynamodb.UpdateItemInput {
	return &dynamodb.UpdateItemInput{
		TableName: aws.String(p.table), Key: p.key, UpdateExpression: p.update,
		ConditionExpression: p.condition, ExpressionAttributeNames: p.names,
		ExpressionAttributeValues: p.values, ReturnValues: p.returnVals,
		ReturnValuesOnConditionCheckFailure: p.returnCCF,
	}
}

// conditionFailure reports whether err is a failed condition, returning the
// old item DynamoDB sent back, if any.
func conditionFailure(err error) (bool, map[string]types.AttributeValue) {
	var ccf *types.ConditionalCheckFailedException
	if errors.As(err, &ccf) {
		return true, ccf.Item
	}
	return false, nil
}

func wrapAWS(err error) error {
	if ok, _ := conditionFailure(err); ok {
		return fmt.Errorf("%w: %w", ErrConditionFailed, err)
	}
	return err
}
