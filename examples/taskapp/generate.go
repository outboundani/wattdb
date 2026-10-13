// Command taskapp shows wattdb end to end: a schema in ./schema, the code
// generated from it in ./db, and a program using the generated clients.
//
// Regenerate with: go generate ./examples/...
package main

//go:generate go run github.com/outboundani/wattdb/cmd/wattdb generate ./schema
