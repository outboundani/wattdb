package gen

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strconv"
	"strings"

	"github.com/outboundani/wattdb"
)

// Options configure a run.
type Options struct {
	// ModuleRoot and ModulePath describe the module holding the schemas.
	ModuleRoot string
	ModulePath string
	// SchemaDir holds one schema package per service (schema/<service>).
	SchemaDir string
	// OutDir receives db/<entity>, db/runtime; DBImport is its import path.
	OutDir   string
	DBImport string
	// Check validates without writing files.
	Check bool
}

// Schema is one schema value handed to Main by the phase-2 program.
type Schema struct {
	Pkg   string
	Type  string
	Ptr   bool
	Value wattdb.Schema
}

// Generate runs both phases: it reads the schema source, makes the entity
// types available through a go overlay, then runs a generated program that
// loads the schemas and calls Main.
func Generate(opts Options) error {
	pkgs, err := schemaPackages(opts)
	if err != nil {
		return err
	}
	if len(pkgs) == 0 {
		return fmt.Errorf("gen: no schema packages under %s", opts.SchemaDir)
	}

	tmp, err := os.MkdirTemp("", "wattdb-gen-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)

	overlay := map[string]string{}
	var schemas []*entity
	seen := map[string]string{}
	for _, p := range pkgs {
		src, err := parseSchemaDir(p.dir, p.importPath)
		if err != nil {
			return err
		}
		for _, e := range src.Entities {
			if prev, dup := seen[e.Pkg]; dup {
				return fmt.Errorf("gen: %s and %s.%s both generate package %s", prev, e.SchemaPkg, e.SchemaType, e.Pkg)
			}
			seen[e.Pkg] = e.SchemaPkg + "." + e.SchemaType
			out, err := renderTypes(e)
			if err != nil {
				return err
			}
			dir := filepath.Join(opts.OutDir, e.Pkg)
			file := filepath.Join(tmp, e.Pkg+"_types.go")
			if err := os.WriteFile(file, out, 0o644); err != nil {
				return err
			}
			overlay[filepath.Join(dir, "types.go")] = file
			// Hide other generated files: they may be stale.
			stale, err := generatedFiles(dir)
			if err != nil {
				return err
			}
			for _, f := range stale {
				if filepath.Base(f) != "types.go" {
					overlay[f] = ""
				}
			}
			schemas = append(schemas, e)
		}
	}
	if len(schemas) == 0 {
		return fmt.Errorf("gen: no schema types (types with Model, Attributes and Indexes methods) under %s", opts.SchemaDir)
	}

	mainDir, err := os.MkdirTemp(opts.ModuleRoot, "_wattdbgen")
	if err != nil {
		return err
	}
	defer os.RemoveAll(mainDir)
	if err := os.WriteFile(filepath.Join(mainDir, "main.go"), phase2Main(opts, schemas), 0o644); err != nil {
		return err
	}
	ov, err := json.Marshal(map[string]any{"Replace": overlay})
	if err != nil {
		return err
	}
	ovFile := filepath.Join(tmp, "overlay.json")
	if err := os.WriteFile(ovFile, ov, 0o644); err != nil {
		return err
	}

	cmd := exec.Command("go", "run", "-overlay", ovFile, "./"+filepath.Base(mainDir))
	cmd.Dir = opts.ModuleRoot
	var stderr bytes.Buffer
	cmd.Stdout = os.Stdout
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("gen: phase 2 failed: %v\n%s", err, strings.TrimSpace(stderr.String()))
	}
	return nil
}

type schemaPkg struct {
	dir, importPath string
}

// schemaPackages finds the Go packages under SchemaDir.
func schemaPackages(opts Options) ([]schemaPkg, error) {
	var out []schemaPkg
	err := filepath.WalkDir(opts.SchemaDir, func(path string, d os.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if !d.IsDir() {
			return nil
		}
		name := d.Name()
		if path != opts.SchemaDir && (strings.HasPrefix(name, ".") || strings.HasPrefix(name, "_") || name == "testdata") {
			return filepath.SkipDir
		}
		matches, _ := filepath.Glob(filepath.Join(path, "*.go"))
		if len(matches) == 0 {
			return nil
		}
		rel, err := filepath.Rel(opts.ModuleRoot, path)
		if err != nil {
			return err
		}
		out = append(out, schemaPkg{dir: path, importPath: opts.ModulePath + "/" + filepath.ToSlash(rel)})
		return nil
	})
	return out, err
}

// generatedFiles lists the files in dir that wattdb generated.
func generatedFiles(dir string) ([]string, error) {
	entries, err := os.ReadDir(dir)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	var out []string
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".go") {
			continue
		}
		path := filepath.Join(dir, e.Name())
		b, err := os.ReadFile(path)
		if err != nil {
			return nil, err
		}
		if bytes.HasPrefix(b, []byte(header[:len(header)-2])) {
			out = append(out, path)
		}
	}
	return out, nil
}

func phase2Main(opts Options, schemas []*entity) []byte {
	var w emitter
	w.WriteString(header)
	w.p("package main\n")
	aliases := map[string]string{}
	var pkgs []string
	for _, e := range schemas {
		if _, ok := aliases[e.SchemaPkg]; !ok {
			aliases[e.SchemaPkg] = ""
			pkgs = append(pkgs, e.SchemaPkg)
		}
	}
	sort.Strings(pkgs)
	w.p("import (")
	w.p("\t\"github.com/outboundani/wattdb/gen\"")
	for i, p := range pkgs {
		aliases[p] = "schema" + strconv.Itoa(i)
		w.p("\t%s %q", aliases[p], p)
	}
	w.p(")\n")
	w.p("func main() {")
	w.p("\tgen.Main(gen.Options{")
	w.p("\t\tModuleRoot: %q,", opts.ModuleRoot)
	w.p("\t\tModulePath: %q,", opts.ModulePath)
	w.p("\t\tSchemaDir:  %q,", opts.SchemaDir)
	w.p("\t\tOutDir:     %q,", opts.OutDir)
	w.p("\t\tDBImport:   %q,", opts.DBImport)
	w.p("\t\tCheck:      %v,", opts.Check)
	w.p("\t},")
	for _, e := range schemas {
		v := fmt.Sprintf("%s.%s{}", aliases[e.SchemaPkg], e.SchemaType)
		if e.SchemaPtr {
			v = "&" + v
		}
		w.p("\t\tgen.Schema{Pkg: %q, Type: %q, Ptr: %v, Value: %s},", e.SchemaPkg, e.SchemaType, e.SchemaPtr, v)
	}
	w.p("\t)")
	w.p("}")
	return w.Bytes()
}

// Main is phase 2. The program Generate writes calls it with every schema;
// it validates them and writes the generated packages.
func Main(opts Options, schemas ...Schema) {
	if err := run(opts, schemas); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(opts Options, schemas []Schema) error {
	var entities []*entity
	var errs []string
	for _, s := range schemas {
		if err := wattdb.Check(s.Value); err != nil {
			errs = append(errs, err.Error())
			continue
		}
		e, err := fromSchema(s.Pkg, s.Type, s.Ptr, s.Value)
		if err != nil {
			errs = append(errs, err.Error())
			continue
		}
		if err := checkCallbacks(e, s.Value); err != nil {
			errs = append(errs, err.Error())
			continue
		}
		entities = append(entities, e)
	}
	errs = append(errs, checkCollections(schemas)...)
	if len(errs) > 0 {
		return errors.New(strings.Join(errs, "\n"))
	}

	files := map[string][]byte{}
	for _, e := range entities {
		types, err := renderTypes(e)
		if err != nil {
			return err
		}
		ent, err := renderEntity(e, opts.DBImport)
		if err != nil {
			return err
		}
		dir := filepath.Join(opts.OutDir, e.Pkg)
		files[filepath.Join(dir, "types.go")] = types
		files[filepath.Join(dir, "entity.go")] = ent
	}
	var services []string
	byService := map[string][]*entity{}
	for _, e := range entities {
		if _, ok := byService[e.Service]; !ok {
			services = append(services, e.Service)
		}
		byService[e.Service] = append(byService[e.Service], e)
	}
	pkgs := map[string]string{}
	for _, e := range entities {
		pkgs[e.Pkg] = "entity " + e.Name
	}
	for _, svc := range services {
		colls := serviceCollections(byService[svc])
		if len(colls) == 0 {
			continue
		}
		pkg := packageName(svc, "")
		if prev, ok := pkgs[pkg]; ok {
			return fmt.Errorf("service %q generates package %s, which %s also generates", svc, pkg, prev)
		}
		pkgs[pkg] = "service " + svc
		src, err := renderService(pkg, byService[svc], colls, opts.DBImport)
		if err != nil {
			return err
		}
		files[filepath.Join(opts.OutDir, pkg, "collections.go")] = src
	}
	for e, old := range migrations(entities) {
		src, err := renderMigration(e, old, opts.DBImport)
		if err != nil {
			return err
		}
		files[filepath.Join(opts.OutDir, e.Pkg, "migrate.go")] = src
	}
	rt, err := renderRuntime(entities, opts.DBImport)
	if err != nil {
		return err
	}
	files[filepath.Join(opts.OutDir, "runtime", "runtime.go")] = rt

	if opts.Check {
		return nil
	}
	return writeFiles(files)
}

// writeFiles writes the generated files and removes generated files that
// are no longer produced from the same directories.
func writeFiles(files map[string][]byte) error {
	dirs := map[string]bool{}
	for path, content := range files {
		dir := filepath.Dir(path)
		dirs[dir] = true
		if err := os.MkdirAll(dir, 0o755); err != nil {
			return err
		}
		if err := os.WriteFile(path, content, 0o644); err != nil {
			return err
		}
	}
	for dir := range dirs {
		existing, err := generatedFiles(dir)
		if err != nil {
			return err
		}
		for _, f := range existing {
			if _, keep := files[f]; !keep {
				if err := os.Remove(f); err != nil {
					return err
				}
			}
		}
	}
	return nil
}

// checkCollections checks that entities sharing a service define each
// collection on the same index with the same key fields and layout.
func checkCollections(schemas []Schema) []string {
	type use struct {
		entity, pattern, index, pk, sk string
		typ                            string
	}
	seen := map[string]use{} // service + "\x00" + collection
	var errs []string
	for _, s := range schemas {
		m := s.Value.Model()
		for _, ix := range s.Value.Indexes() {
			d := ix.Descriptor()
			u := use{entity: m.Entity, pattern: d.AccessPattern, index: d.Index, pk: d.PK.Field, typ: string(d.Type)}
			if d.SK != nil {
				u.sk = d.SK.Field
			}
			if u.typ == "" {
				u.typ = "isolated"
			}
			for _, c := range d.Collection {
				key := m.Service.Name() + "\x00" + c
				prev, ok := seen[key]
				if !ok {
					seen[key] = u
					continue
				}
				if prev.entity == u.entity {
					continue // ElectroDB reports duplicates within an entity
				}
				if prev.index != u.index || prev.pk != u.pk || prev.sk != u.sk || prev.typ != u.typ {
					errs = append(errs, fmt.Sprintf("collection %q in service %q: entity %q defines it on index %q (pk %s, sk %s, %s) but entity %q on index %q (pk %s, sk %s, %s)",
						c, m.Service.Name(), prev.entity, prev.index, prev.pk, prev.sk, prev.typ, u.entity, u.index, u.pk, u.sk, u.typ))
				}
			}
		}
	}
	return errs
}
