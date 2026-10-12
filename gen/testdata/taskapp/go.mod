module example.com/taskapp

go 1.27.1

require (
	github.com/aws/aws-sdk-go-v2/feature/dynamodb/attributevalue v1.21.11
	github.com/aws/aws-sdk-go-v2/service/dynamodb v1.70.3
	github.com/outboundani/wattdb v0.0.0
)

require (
	github.com/aws/aws-sdk-go-v2/service/dynamodbstreams v1.45.1 // indirect
	github.com/aws/smithy-go v1.28.5 // indirect
)

replace github.com/outboundani/wattdb => ../../..
