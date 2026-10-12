package wattdb

import (
	"errors"

	"github.com/outboundani/wattdb/internal/engine"
)

// Errors (decision D48). Match them with errors.Is; AWS errors are wrapped
// with %w and remain reachable with errors.As.
var (
	// ErrNotFound is returned by Get when the item doesn't exist, and when a
	// Returning call got no item back.
	ErrNotFound = errors.New("wattdb: item not found")
	// ErrConditionFailed is returned when a write's condition failed.
	ErrConditionFailed = errors.New("wattdb: condition failed")
	// ErrValidation is returned for invalid attribute values; the error is a
	// *ValidationError.
	ErrValidation = engine.ErrValidation
	// ErrIncompleteComposites is returned when a key or an index key can't
	// be built from what was given; the error is an
	// *IncompleteCompositesError.
	ErrIncompleteComposites = engine.ErrIncompleteComposites
)

// ValidationError lists invalid attribute values.
type ValidationError = engine.ValidationError

// FieldError is one invalid attribute value: Path, Reason, the list Index
// where relevant, and the Cause returned by a Validate callback.
type FieldError = engine.FieldError

// IncompleteCompositesError names the composites a key or index needed.
type IncompleteCompositesError = engine.IncompleteCompositesError
