// Package codec converts Go values to and from stored DynamoDB values
// (decision D18). Time, Duration and UUID attributes use the defaults below
// unless a schema sets another codec; Custom attributes always name one.
package codec

import (
	"encoding/hex"
	"fmt"
	"strconv"
	"strings"
	"time"

	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
)

// Type is the ElectroDB attribute type a codec stores.
type Type string

const (
	String  Type = "string"
	Number  Type = "number"
	Boolean Type = "boolean"
	// Any skips ElectroDB's type check, e.g. for binary values.
	Any Type = "any"
)

// Codec stores T as a DynamoDB value.
type Codec[T any] interface {
	Type() Type
	Encode(v T) (types.AttributeValue, error)
	Decode(av types.AttributeValue) (T, error)
}

// KeyCodec is a Codec that can also render T in a key, for codecs whose
// stored form can't be used in a key directly (e.g. binary).
type KeyCodec[T any] interface {
	Codec[T]
	KeyString(v T) (string, error)
}

var (
	// Time stores UTC RFC 3339 with a fixed 9-digit fraction, so stored
	// times sort lexically. This is the default for Time attributes.
	Time Codec[time.Time] = timeCodec{layout: "2006-01-02T15:04:05.000000000Z"}
	// JSISO stores times the way JS Date.toISOString does (UTC,
	// milliseconds), for fields shared with ElectroDB services.
	JSISO Codec[time.Time] = timeCodec{layout: "2006-01-02T15:04:05.000Z"}
	// Duration stores nanoseconds as a number. This is the default for
	// Duration attributes.
	Duration Codec[time.Duration] = durationCodec{}
	// UUIDBinary stores a UUID as 16 bytes. This is the default for UUID
	// attributes. JS can't read binary through ElectroDB's types.
	UUIDBinary KeyCodec[[16]byte] = uuidBinary{}
	// UUIDString stores a UUID in its canonical lowercase text form.
	UUIDString Codec[[16]byte] = uuidString{}
)

type timeCodec struct{ layout string }

func (timeCodec) Type() Type { return String }

func (c timeCodec) Encode(t time.Time) (types.AttributeValue, error) {
	return &types.AttributeValueMemberS{Value: t.UTC().Format(c.layout)}, nil
}

func (timeCodec) Decode(av types.AttributeValue) (time.Time, error) {
	s, ok := av.(*types.AttributeValueMemberS)
	if !ok {
		return time.Time{}, fmt.Errorf("codec: time needs a string, got %T", av)
	}
	return time.Parse(time.RFC3339Nano, s.Value)
}

type durationCodec struct{}

func (durationCodec) Type() Type { return Number }

func (durationCodec) Encode(d time.Duration) (types.AttributeValue, error) {
	return &types.AttributeValueMemberN{Value: strconv.FormatInt(int64(d), 10)}, nil
}

func (durationCodec) Decode(av types.AttributeValue) (time.Duration, error) {
	n, ok := av.(*types.AttributeValueMemberN)
	if !ok {
		return 0, fmt.Errorf("codec: duration needs a number, got %T", av)
	}
	i, err := strconv.ParseInt(n.Value, 10, 64)
	return time.Duration(i), err
}

type uuidBinary struct{}

func (uuidBinary) Type() Type { return Any }

func (uuidBinary) Encode(u [16]byte) (types.AttributeValue, error) {
	return &types.AttributeValueMemberB{Value: u[:]}, nil
}

func (uuidBinary) Decode(av types.AttributeValue) ([16]byte, error) {
	var u [16]byte
	b, ok := av.(*types.AttributeValueMemberB)
	if !ok || len(b.Value) != 16 {
		return u, fmt.Errorf("codec: uuid needs 16 bytes, got %T", av)
	}
	copy(u[:], b.Value)
	return u, nil
}

func (uuidBinary) KeyString(u [16]byte) (string, error) { return formatUUID(u), nil }

type uuidString struct{}

func (uuidString) Type() Type { return String }

func (uuidString) Encode(u [16]byte) (types.AttributeValue, error) {
	return &types.AttributeValueMemberS{Value: formatUUID(u)}, nil
}

func (uuidString) Decode(av types.AttributeValue) ([16]byte, error) {
	var u [16]byte
	s, ok := av.(*types.AttributeValueMemberS)
	if !ok {
		return u, fmt.Errorf("codec: uuid needs a string, got %T", av)
	}
	b, err := hex.DecodeString(strings.ReplaceAll(s.Value, "-", ""))
	if err != nil || len(b) != 16 {
		return u, fmt.Errorf("codec: invalid uuid %q", s.Value)
	}
	copy(u[:], b)
	return u, nil
}

func formatUUID(u [16]byte) string {
	h := hex.EncodeToString(u[:])
	return h[0:8] + "-" + h[8:12] + "-" + h[12:16] + "-" + h[16:20] + "-" + h[20:32]
}
