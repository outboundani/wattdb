package attr

import "fmt"

func errorf(format string, args ...any) error {
	return fmt.Errorf("attr: "+format, args...)
}
