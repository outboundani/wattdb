// Command wattdb generates typed DynamoDB packages from wattdb schemas.
package main

import (
	"fmt"
	"os"
)

const usage = `usage: wattdb <command> [arguments]

commands:
  generate <dir>   generate packages from the schemas in dir
  check <dir>      validate the schemas in dir without writing files
`

func main() {
	if len(os.Args) < 2 {
		fmt.Fprint(os.Stderr, usage)
		os.Exit(2)
	}
	switch os.Args[1] {
	case "generate", "check":
		fmt.Fprintf(os.Stderr, "wattdb: %s is not implemented yet\n", os.Args[1])
		os.Exit(1)
	default:
		fmt.Fprintf(os.Stderr, "wattdb: unknown command %q\n\n%s", os.Args[1], usage)
		os.Exit(2)
	}
}
