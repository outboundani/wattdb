package engine

import (
	"errors"
	"fmt"
	"strings"
)

// Sentinel errors. The public wattdb package re-exports these (decision D48).
var (
	ErrIncompleteComposites = errors.New("wattdb: incomplete composite attributes")
	ErrValidation           = errors.New("wattdb: validation failed")
)

// IncompleteCompositesError reports key composites a write or lookup needs but
// didn't get.
type IncompleteCompositesError struct {
	// AccessPatterns lists the affected access patterns, if known.
	AccessPatterns []string
	Missing        []string
	// Conditional is set when a sparse index (one with a condition) was
	// given some but not all of its composites.
	Conditional bool
}

func (e *IncompleteCompositesError) Error() string {
	msg := fmt.Sprintf("wattdb: missing composite attributes %s", strings.Join(e.Missing, ", "))
	if len(e.AccessPatterns) > 0 {
		msg += fmt.Sprintf(" for access patterns %s", strings.Join(e.AccessPatterns, ", "))
	}
	return msg
}

func (e *IncompleteCompositesError) Unwrap() error { return ErrIncompleteComposites }

// FieldError is one invalid attribute value.
type FieldError struct {
	Path   string
	Reason string
	// Index is the list index the error occurred at, if any.
	Index *int
	Cause error
}

// ValidationError reports invalid attribute values.
type ValidationError struct {
	Fields []FieldError
}

func (e *ValidationError) Error() string {
	parts := make([]string, len(e.Fields))
	for i, f := range e.Fields {
		parts[i] = fmt.Sprintf("%s: %s", f.Path, f.Reason)
	}
	return "wattdb: invalid attributes: " + strings.Join(parts, "; ")
}

func (e *ValidationError) Unwrap() error { return ErrValidation }
