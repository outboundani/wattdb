package gen

import (
	"bytes"
	"fmt"
	"go/ast"
	"go/parser"
	"go/printer"
	"go/token"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"

	"github.com/outboundani/wattdb/attr"
)

const (
	wattdbImport = "github.com/outboundani/wattdb"
	attrImport   = "github.com/outboundani/wattdb/attr"
)

// schemaSource is one parsed schema package.
type schemaSource struct {
	Dir        string
	ImportPath string
	Entities   []*entity
}

// parseSchemaDir reads the schema types in one package directory (phase 1).
// Attribute names, kinds and types must be written literally in
// Attributes(), so they can be read without compiling the package.
func parseSchemaDir(dir, importPath string) (*schemaSource, error) {
	fset := token.NewFileSet()
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}
	var files []*ast.File
	for _, de := range entries {
		name := de.Name()
		if de.IsDir() || !strings.HasSuffix(name, ".go") || strings.HasSuffix(name, "_test.go") {
			continue
		}
		f, err := parser.ParseFile(fset, filepath.Join(dir, name), nil, parser.SkipObjectResolution)
		if err != nil {
			return nil, err
		}
		files = append(files, f)
	}

	type methods struct {
		decls map[string]*ast.FuncDecl
		file  *ast.File
		ptr   bool
	}
	types := map[string]*methods{}
	for _, f := range files {
		for _, decl := range f.Decls {
			fd, ok := decl.(*ast.FuncDecl)
			if !ok || fd.Recv == nil || len(fd.Recv.List) != 1 {
				continue
			}
			name, ptr := receiverType(fd.Recv.List[0].Type)
			if name == "" {
				continue
			}
			switch fd.Name.Name {
			case "Model", "Attributes", "Indexes":
			default:
				continue
			}
			m := types[name]
			if m == nil {
				m = &methods{decls: map[string]*ast.FuncDecl{}}
				types[name] = m
			}
			m.decls[fd.Name.Name] = fd
			m.ptr = m.ptr || ptr
			if fd.Name.Name == "Attributes" {
				m.file = f
			}
		}
	}

	src := &schemaSource{Dir: dir, ImportPath: importPath}
	var names []string
	for name, m := range types {
		if len(m.decls) == 3 {
			names = append(names, name)
		}
	}
	sort.Strings(names)
	for _, name := range names {
		m := types[name]
		e := &entity{SchemaPkg: importPath, SchemaType: name, SchemaPtr: m.ptr}
		p := &astParser{fset: fset, imports: fileImports(m.file)}
		if err := p.model(e, m.decls["Model"]); err != nil {
			return nil, fmt.Errorf("%s.%s: %w", importPath, name, err)
		}
		if err := p.attributes(e, m.decls["Attributes"]); err != nil {
			return nil, fmt.Errorf("%s.%s: %w", importPath, name, err)
		}
		e.Pkg = packageName(e.Name, e.Version)
		if err := e.assignNames(); err != nil {
			return nil, err
		}
		src.Entities = append(src.Entities, e)
	}
	return src, nil
}

func receiverType(expr ast.Expr) (string, bool) {
	ptr := false
	if s, ok := expr.(*ast.StarExpr); ok {
		ptr, expr = true, s.X
	}
	if id, ok := expr.(*ast.Ident); ok {
		return id.Name, ptr
	}
	return "", false
}

// fileImports maps each import's local name to its path.
func fileImports(f *ast.File) map[string]string {
	out := map[string]string{}
	if f == nil {
		return out
	}
	for _, imp := range f.Imports {
		path, _ := strconv.Unquote(imp.Path.Value)
		name := path[strings.LastIndex(path, "/")+1:]
		if imp.Name != nil {
			name = imp.Name.Name
		}
		out[name] = path
	}
	return out
}

type astParser struct {
	fset    *token.FileSet
	imports map[string]string
}

func (p *astParser) errorf(n ast.Node, format string, args ...any) error {
	return fmt.Errorf("%s: %s", p.fset.Position(n.Pos()), fmt.Sprintf(format, args...))
}

func (p *astParser) importName(path string) string {
	for name, ip := range p.imports {
		if ip == path {
			return name
		}
	}
	return ""
}

// returnExpr finds the single returned expression of a method.
func (p *astParser) returnExpr(fd *ast.FuncDecl) (ast.Expr, error) {
	var ret *ast.ReturnStmt
	for _, st := range fd.Body.List {
		if r, ok := st.(*ast.ReturnStmt); ok {
			ret = r
		}
	}
	if ret == nil || len(ret.Results) != 1 {
		return nil, p.errorf(fd, "%s must return a single literal", fd.Name.Name)
	}
	return ret.Results[0], nil
}

func (p *astParser) model(e *entity, fd *ast.FuncDecl) error {
	expr, err := p.returnExpr(fd)
	if err != nil {
		return err
	}
	lit, ok := expr.(*ast.CompositeLit)
	if !ok {
		return p.errorf(expr, "Model must return a wattdb.Model literal")
	}
	for _, elt := range lit.Elts {
		kv, ok := elt.(*ast.KeyValueExpr)
		if !ok {
			return p.errorf(elt, "Model fields must be named")
		}
		key, _ := kv.Key.(*ast.Ident)
		if key == nil {
			continue
		}
		switch key.Name {
		case "Entity", "Version":
			s, err := p.stringLit(kv.Value)
			if err != nil {
				return err
			}
			if key.Name == "Entity" {
				e.Name = s
			} else {
				e.Version = s
			}
		}
	}
	if e.Name == "" {
		return p.errorf(lit, "Model needs a literal Entity")
	}
	return nil
}

func (p *astParser) stringLit(expr ast.Expr) (string, error) {
	bl, ok := expr.(*ast.BasicLit)
	if !ok || bl.Kind != token.STRING {
		return "", p.errorf(expr, "expected a string literal")
	}
	return strconv.Unquote(bl.Value)
}

func (p *astParser) attributes(e *entity, fd *ast.FuncDecl) error {
	expr, err := p.returnExpr(fd)
	if err != nil {
		return err
	}
	lit, ok := expr.(*ast.CompositeLit)
	if !ok {
		return p.errorf(expr, "Attributes must return a slice literal")
	}
	for _, elt := range lit.Elts {
		a, err := p.attribute(elt)
		if err != nil {
			return err
		}
		e.Attrs = append(e.Attrs, a)
	}
	return nil
}

// attribute reads one attr builder chain, e.g.
// attr.Enum("Role", "a", "b").Field("role").Required().
func (p *astParser) attribute(expr ast.Expr) (*attribute, error) {
	var methods []*ast.CallExpr
	cur := expr
	for {
		call, ok := cur.(*ast.CallExpr)
		if !ok {
			return nil, p.errorf(expr, "attributes must be attr builder calls")
		}
		fun := call.Fun
		var typeArgs []ast.Expr
		switch f := fun.(type) {
		case *ast.IndexExpr:
			fun, typeArgs = f.X, []ast.Expr{f.Index}
		case *ast.IndexListExpr:
			fun, typeArgs = f.X, f.Indices
		}
		sel, ok := fun.(*ast.SelectorExpr)
		if !ok {
			return nil, p.errorf(call, "attributes must be attr builder calls")
		}
		if pkg, ok := sel.X.(*ast.Ident); ok && p.imports[pkg.Name] == attrImport {
			return p.constructor(sel.Sel.Name, call, typeArgs, methods)
		}
		methods = append(methods, call)
		cur = sel.X
	}
}

func (p *astParser) constructor(name string, call *ast.CallExpr, typeArgs []ast.Expr, methods []*ast.CallExpr) (*attribute, error) {
	kinds := map[string]attr.Kind{
		"String": attr.KindString, "Int": attr.KindInt, "Int64": attr.KindInt64,
		"Float64": attr.KindFloat64, "Bool": attr.KindBool, "Enum": attr.KindEnum,
		"Time": attr.KindTime, "Duration": attr.KindDuration, "UUID": attr.KindUUID,
		"Set": attr.KindSet, "List": attr.KindList, "Map": attr.KindMap,
		"Any": attr.KindAny, "AnyOf": attr.KindAnyOf, "Custom": attr.KindCustom,
	}
	kind, ok := kinds[name]
	if !ok {
		return nil, p.errorf(call, "unknown attr constructor %s", name)
	}
	if len(call.Args) == 0 {
		return nil, p.errorf(call, "attr.%s needs a name", name)
	}
	goName, err := p.stringLit(call.Args[0])
	if err != nil {
		return nil, err
	}
	a := &attribute{GoName: goName, Kind: kind}

	switch kind {
	case attr.KindEnum:
		for _, arg := range call.Args[1:] {
			v, err := p.stringLit(arg)
			if err != nil {
				return nil, err
			}
			a.Enum = append(a.Enum, v)
		}
	case attr.KindList:
		if len(call.Args) != 2 {
			return nil, p.errorf(call, "attr.List takes a name and an item")
		}
		items, err := p.attribute(call.Args[1])
		if err != nil {
			return nil, err
		}
		items.GoName = itemName(items)
		a.Items = items
	case attr.KindMap:
		for _, arg := range call.Args[1:] {
			prop, err := p.attribute(arg)
			if err != nil {
				return nil, err
			}
			a.Props = append(a.Props, prop)
		}
	case attr.KindSet, attr.KindAnyOf, attr.KindCustom:
		if len(typeArgs) != 1 {
			return nil, p.errorf(call, "attr.%s needs a type argument", name)
		}
		ref, err := p.typeRef(typeArgs[0])
		if err != nil {
			return nil, err
		}
		a.Elem = ref
	}

	// Methods were collected outermost first.
	for i := len(methods) - 1; i >= 0; i-- {
		m := methods[i]
		sel := m.Fun.(*ast.SelectorExpr)
		switch sel.Sel.Name {
		case "Field", "JSONName":
			if len(m.Args) != 1 {
				return nil, p.errorf(m, "%s takes one argument", sel.Sel.Name)
			}
			s, err := p.stringLit(m.Args[0])
			if err != nil {
				return nil, err
			}
			if sel.Sel.Name == "Field" {
				a.Field = s
			} else {
				a.JSON = s
			}
		case "Required":
			a.Required = true
		case "Default":
			a.HasDefault = true
		case "DefaultFunc":
			a.HasDefaultFunc = true
		case "Get":
			a.HasGet = true
		case "Set":
			a.HasSet = true
		case "Validate":
			a.HasValidate = true
		case "Codec":
			a.HasCodec = true
		}
	}
	return a, nil
}

// itemName keeps a list item's builder name only where it names a type.
func itemName(items *attribute) string {
	if items.Kind == attr.KindMap {
		return items.GoName
	}
	return ""
}

func (p *astParser) typeRef(expr ast.Expr) (typeRef, error) {
	var buf bytes.Buffer
	if err := printer.Fprint(&buf, token.NewFileSet(), expr); err != nil {
		return typeRef{}, err
	}
	r := typeRef{Expr: buf.String()}
	switch r.Expr {
	case "int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16", "uint32", "uint64", "float32", "float64":
		r.Numeric = true
	}
	var err error
	ast.Inspect(expr, func(n ast.Node) bool {
		sel, ok := n.(*ast.SelectorExpr)
		if !ok {
			return true
		}
		if id, ok := sel.X.(*ast.Ident); ok {
			path, ok := p.imports[id.Name]
			if !ok {
				err = p.errorf(sel, "unknown package %s", id.Name)
			}
			r.Import = path
		}
		return false
	})
	return r, err
}
