// Package attr provides the attribute builders used in wattdb schemas.
//
// Each constructor takes the attribute's Go name, which names the generated
// struct field. The stored DynamoDB field (also the ElectroDB attribute name)
// defaults to the Go name and is set with Field:
//
//	attr.String("UserID").Field("userId").Required()
//
// Builder methods mirror ElectroDB's attribute properties (decision D13).
// Callbacks are typed against the generated entity package (decision D11);
// the generator checks their signatures.
package attr

import (
	"reflect"
	"regexp"
)

// Kind is the attribute constructor that was used.
type Kind string

const (
	KindString   Kind = "string"
	KindInt      Kind = "int"
	KindInt64    Kind = "int64"
	KindFloat64  Kind = "float64"
	KindBool     Kind = "bool"
	KindEnum     Kind = "enum"
	KindTime     Kind = "time"
	KindDuration Kind = "duration"
	KindUUID     Kind = "uuid"
	KindSet      Kind = "set"
	KindList     Kind = "list"
	KindMap      Kind = "map"
	KindAny      Kind = "any"
	KindAnyOf    Kind = "anyOf"
	KindCustom   Kind = "custom"
)

// Attribute is an attribute definition. Schemas return them from Attributes.
type Attribute interface {
	Descriptor() *Descriptor
}

// Padding pads key composites, like ElectroDB's padding property.
type Padding struct {
	Length int
	Char   string
}

// Descriptor holds everything a builder recorded. The generator and
// wattdb.Compile read it; schemas don't use it directly.
type Descriptor struct {
	Name     string
	Kind     Kind
	Field    string
	Label    *string
	JSONName string
	Required bool
	Hidden   bool
	ReadOnly bool
	Padding  *Padding

	// Default is the value given to Default; HasDefault reports whether it
	// was called. DefaultFunc is the function given to DefaultFunc.
	Default     any
	HasDefault  bool
	DefaultFunc any

	Validate       any
	ValidateRegexp *regexp.Regexp
	Get            any
	Set            any

	Watch    []string
	WatchAll bool

	Enum  []string
	Items *Descriptor
	Props []*Descriptor

	// GoType is the type argument of Set, AnyOf and Custom.
	GoType reflect.Type
	// Codec is the codec given to Codec or Custom, if any.
	Codec any

	// Err is the first error a builder method recorded (decision D49).
	Err error
}

// Builder builds one attribute.
type Builder struct {
	d Descriptor
}

// Descriptor returns the recorded definition.
func (b *Builder) Descriptor() *Descriptor { return &b.d }

func newBuilder(name string, kind Kind) *Builder {
	return &Builder{d: Descriptor{Name: name, Kind: kind}}
}

// String is a string attribute.
func String(name string) *Builder { return newBuilder(name, KindString) }

// Int is a number attribute held as int.
func Int(name string) *Builder { return newBuilder(name, KindInt) }

// Int64 is a number attribute held as int64.
func Int64(name string) *Builder { return newBuilder(name, KindInt64) }

// Float64 is a number attribute held as float64.
func Float64(name string) *Builder { return newBuilder(name, KindFloat64) }

// Bool is a boolean attribute.
func Bool(name string) *Builder { return newBuilder(name, KindBool) }

// Enum is a string attribute limited to values. The generated type is
// <Name>Enum with a constant per value.
func Enum(name string, values ...string) *Builder {
	b := newBuilder(name, KindEnum)
	b.d.Enum = values
	return b
}

// Time is a time.Time attribute, stored by default as UTC RFC 3339 with a
// fixed 9-digit fraction (decision D18).
func Time(name string) *Builder { return newBuilder(name, KindTime) }

// Duration is a time.Duration attribute, stored by default as nanoseconds.
func Duration(name string) *Builder { return newBuilder(name, KindDuration) }

// UUID is a [16]byte attribute, stored by default as binary.
func UUID(name string) *Builder { return newBuilder(name, KindUUID) }

// Set is a DynamoDB set of T (a string or number type), held as []T.
func Set[T ~string | ~int | ~int64 | ~float64](name string) *Builder {
	b := newBuilder(name, KindSet)
	b.d.GoType = reflect.TypeFor[T]()
	return b
}

// List is a list whose items are described by item. The item builder's name
// is ignored, except for maps, where it names the generated type.
func List(name string, item Attribute) *Builder {
	b := newBuilder(name, KindList)
	if item == nil {
		b.fail("List %q needs an item", name)
		return b
	}
	b.d.Items = item.Descriptor()
	return b
}

// Map is a map with the given properties. The generated type is named after
// the map's path plus "Map".
func Map(name string, props ...Attribute) *Builder {
	b := newBuilder(name, KindMap)
	b.d.Props = []*Descriptor{}
	for _, p := range props {
		b.d.Props = append(b.d.Props, p.Descriptor())
	}
	return b
}

// Any holds any value, as a Go any.
func Any(name string) *Builder { return newBuilder(name, KindAny) }

// AnyOf holds any value, decoded into T with attributevalue.
func AnyOf[T any](name string) *Builder {
	b := newBuilder(name, KindAnyOf)
	b.d.GoType = reflect.TypeFor[T]()
	return b
}

// Custom holds a T stored through codec, which must implement
// codec.Codec[T].
func Custom[T any](name string, codec any) *Builder {
	b := newBuilder(name, KindCustom)
	b.d.GoType = reflect.TypeFor[T]()
	b.d.Codec = codec
	return b
}

// Field sets the stored DynamoDB field, which is also the ElectroDB
// attribute name. It defaults to the Go name.
func (b *Builder) Field(field string) *Builder { b.d.Field = field; return b }

// Label sets the key label used when the attribute is a key composite.
func (b *Builder) Label(label string) *Builder { b.d.Label = &label; return b }

// JSONName overrides the generated struct's json tag name, which defaults to
// the field.
func (b *Builder) JSONName(name string) *Builder { b.d.JSONName = name; return b }

// Required makes the attribute required on writes.
func (b *Builder) Required() *Builder { b.d.Required = true; return b }

// Hidden leaves the attribute out of reads.
func (b *Builder) Hidden() *Builder { b.d.Hidden = true; return b }

// ReadOnly forbids updating the attribute after it's written.
func (b *Builder) ReadOnly() *Builder { b.d.ReadOnly = true; return b }

// Padding pads the attribute's string form in keys to length with char.
func (b *Builder) Padding(length int, char string) *Builder {
	b.d.Padding = &Padding{Length: length, Char: char}
	return b
}

// Default sets a default value, of the attribute's Go type.
func (b *Builder) Default(v any) *Builder {
	b.d.Default, b.d.HasDefault = v, true
	return b
}

// DefaultFunc sets a function returning the default value: func() T.
func (b *Builder) DefaultFunc(fn any) *Builder { b.d.DefaultFunc = fn; return b }

// Validate sets a validation function: func(*T) error, where nil means the
// value is missing. A non-nil error rejects the value.
func (b *Builder) Validate(fn any) *Builder { b.d.Validate = fn; return b }

// ValidateRegexp rejects values that don't match pattern (Go regexp syntax,
// decision D61).
func (b *Builder) ValidateRegexp(pattern string) *Builder {
	re, err := regexp.Compile(pattern)
	if err != nil {
		b.fail("attribute %q: %v", b.d.Name, err)
		return b
	}
	b.d.ValidateRegexp = re
	return b
}

// Get sets a read transform: func(*T, <entity>.Partial) *T.
func (b *Builder) Get(fn any) *Builder { b.d.Get = fn; return b }

// Set sets a write transform: func(*T, <entity>.Partial) *T.
func (b *Builder) Set(fn any) *Builder { b.d.Set = fn; return b }

// Watch runs this attribute's Get/Set after the named attributes' (Go
// names), whenever they're present.
func (b *Builder) Watch(names ...string) *Builder { b.d.Watch = names; return b }

// WatchAll runs this attribute's Get/Set after every other attribute's.
func (b *Builder) WatchAll() *Builder { b.d.WatchAll = true; return b }

// Codec replaces the default codec for Time, Duration and UUID attributes.
func (b *Builder) Codec(c any) *Builder { b.d.Codec = c; return b }

func (b *Builder) fail(format string, args ...any) {
	if b.d.Err == nil {
		b.d.Err = errorf(format, args...)
	}
}
