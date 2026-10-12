// Package integration holds wattdb's DynamoDB Local tests (decision D63).
// They need Docker and run with: go test -tags integration ./integration/...
package integration

//go:generate go run github.com/outboundani/wattdb/cmd/wattdb generate ./schema
