// Package taskapp holds the example service's schemas. The doc and task
// entities match the recorded ElectroDB vectors (testdata/vectors).
package taskapp

import "github.com/outboundani/wattdb"

// TaskApp is the service every entity here belongs to.
var TaskApp = wattdb.Service("taskapp")

func deref[T any](p *T) T {
	var zero T
	if p == nil {
		return zero
	}
	return *p
}
