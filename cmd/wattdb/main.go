// Command wattdb generates typed DynamoDB packages from wattdb schemas.
//
//	wattdb generate [-out dir] ./schema
//	wattdb check    [-out dir] ./schema
//
// The output directory defaults to "db" next to the schema directory.
package main

import (
	"bufio"
	"errors"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/outboundani/wattdb/gen"
)

const usage = `usage: wattdb <command> [-out dir] <schema dir>

commands:
  generate   generate packages from the schemas
  check      validate the schemas without writing files
`

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, "wattdb:", err)
		os.Exit(1)
	}
}

func run(args []string) error {
	if len(args) < 1 {
		fmt.Fprint(os.Stderr, usage)
		os.Exit(2)
	}
	cmd := args[0]
	if cmd != "generate" && cmd != "check" {
		fmt.Fprintf(os.Stderr, "wattdb: unknown command %q\n\n%s", cmd, usage)
		os.Exit(2)
	}
	fs := flag.NewFlagSet(cmd, flag.ExitOnError)
	out := fs.String("out", "", "output directory (default: db next to the schema directory)")
	fs.Usage = func() { fmt.Fprint(os.Stderr, usage) }
	if err := fs.Parse(args[1:]); err != nil {
		return err
	}
	if fs.NArg() != 1 {
		fs.Usage()
		os.Exit(2)
	}

	schemaDir, err := filepath.Abs(fs.Arg(0))
	if err != nil {
		return err
	}
	outDir := *out
	if outDir == "" {
		outDir = filepath.Join(filepath.Dir(schemaDir), "db")
	}
	if outDir, err = filepath.Abs(outDir); err != nil {
		return err
	}
	root, modPath, err := findModule(schemaDir)
	if err != nil {
		return err
	}
	rel, err := filepath.Rel(root, outDir)
	if err != nil || strings.HasPrefix(rel, "..") {
		return fmt.Errorf("output directory %s must be inside the module at %s", outDir, root)
	}

	opts := gen.Options{
		ModuleRoot: root,
		ModulePath: modPath,
		SchemaDir:  schemaDir,
		OutDir:     outDir,
		DBImport:   strings.TrimSuffix(modPath+"/"+filepath.ToSlash(rel), "/."),
		Check:      cmd == "check",
	}
	if err := gen.Generate(opts); err != nil {
		return err
	}
	if opts.Check {
		fmt.Println("wattdb: schemas are valid")
	}
	return nil
}

// findModule walks up from dir to the enclosing go.mod.
func findModule(dir string) (root, path string, err error) {
	for d := dir; ; d = filepath.Dir(d) {
		f, err := os.Open(filepath.Join(d, "go.mod"))
		if err == nil {
			defer f.Close()
			s := bufio.NewScanner(f)
			for s.Scan() {
				line := strings.TrimSpace(s.Text())
				if mod, ok := strings.CutPrefix(line, "module "); ok {
					return d, strings.Trim(strings.TrimSpace(mod), `"`), nil
				}
			}
			return "", "", fmt.Errorf("%s/go.mod has no module line", d)
		}
		if !errors.Is(err, os.ErrNotExist) {
			return "", "", err
		}
		if filepath.Dir(d) == d {
			return "", "", fmt.Errorf("no go.mod above %s", dir)
		}
	}
}
