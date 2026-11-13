// gocheck prints every type error in the packages of a module, tests included, one per line as
// path:line:col<TAB>message. It type-checks from source and keeps going past a package that
// fails, which `go build` does not.
package main

import (
	"fmt"
	"os"
	"sort"

	"golang.org/x/tools/go/packages"
)

func main() {
	dir := os.Args[1]
	cfg := &packages.Config{
		Mode:  packages.NeedName | packages.NeedFiles | packages.NeedSyntax | packages.NeedTypes | packages.NeedTypesInfo | packages.NeedImports | packages.NeedDeps | packages.NeedModule,
		Dir:   dir,
		Tests: true,
	}
	pkgs, err := packages.Load(cfg, "./...")
	if err != nil {
		fmt.Fprintln(os.Stderr, "load:", err)
		os.Exit(2)
	}
	seen := map[string]bool{}
	var out []string
	packages.Visit(pkgs, nil, func(p *packages.Package) {
		if p.Module == nil || !p.Module.Main {
			return
		}
		for _, e := range p.Errors {
			line := fmt.Sprintf("%s\t%s", e.Pos, e.Msg)
			if !seen[line] {
				seen[line] = true
				out = append(out, line)
			}
		}
	})
	sort.Strings(out)
	for _, l := range out {
		fmt.Println(l)
	}
}
