package main

import (
	"bufio"
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"math"
	"math/rand/v2"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"runtime/debug"
	"strconv"
	"strings"
)

// math/rand/v2's ExpFloat64 calls math.Log on its tail and math.Exp on its
// rejection path, and on arm64 fuses the float32 multiplication and addition of its
// rejection test. The vectors are the draws one call consumes and the bits it returns,
// recorded from Go with a source that forces each path. The expfloat-amd64-nofma file
// holds only the calls that reach math.Exp.
func init() {
	modes["expfloat-tables"] = writeExpTables
	modes["expfloat-amd64-fma"] = func(outDir string) error { return writeExpAMD64(outDir, true) }
	modes["expfloat-amd64-nofma"] = func(outDir string) error { return writeExpAMD64(outDir, false) }
	modes["expfloat-arm64"] = writeExpARM64
}

const expSeed = 701

type expTables struct {
	ke []uint32
	we []float32
	fe []float32
}

// readExpTables reads ke, we and fe from the exp.go of the Go toolchain that runs
// this program, so the tables are Go's own source text.
func readExpTables() (*expTables, error) {
	goroot := os.Getenv("GOROOT")
	if goroot == "" {
		out, err := exec.Command("go", "env", "GOROOT").Output()
		if err != nil {
			return nil, fmt.Errorf("go env GOROOT: %w", err)
		}
		goroot = strings.TrimSpace(string(out))
	}
	path := filepath.Join(goroot, "src", "math", "rand", "v2", "exp.go")
	file, err := parser.ParseFile(token.NewFileSet(), path, nil, 0)
	if err != nil {
		return nil, err
	}
	var t expTables
	for _, decl := range file.Decls {
		gen, ok := decl.(*ast.GenDecl)
		if !ok || gen.Tok != token.VAR {
			continue
		}
		for _, spec := range gen.Specs {
			vs := spec.(*ast.ValueSpec)
			lit, ok := vs.Values[0].(*ast.CompositeLit)
			if !ok {
				continue
			}
			switch vs.Names[0].Name {
			case "ke":
				for _, e := range lit.Elts {
					v, err := strconv.ParseUint(e.(*ast.BasicLit).Value, 0, 32)
					if err != nil {
						return nil, err
					}
					t.ke = append(t.ke, uint32(v))
				}
			case "we", "fe":
				var xs []float32
				for _, e := range lit.Elts {
					v, err := strconv.ParseFloat(e.(*ast.BasicLit).Value, 32)
					if err != nil {
						return nil, err
					}
					xs = append(xs, float32(v))
				}
				if vs.Names[0].Name == "we" {
					t.we = xs
				} else {
					t.fe = xs
				}
			}
		}
	}
	if len(t.ke) != 256 || len(t.we) != 256 || len(t.fe) != 256 {
		return nil, fmt.Errorf("read %d ke, %d we and %d fe entries from %s, want 256 each", len(t.ke), len(t.we), len(t.fe), path)
	}
	return &t, nil
}

func hexU32(v uint32) string {
	return fmt.Sprintf("0x%04x_%04x", v>>16, v&0xffff)
}

func writeExpTables(outDir string) error {
	t, err := readExpTables()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(outDir, 0o755); err != nil {
		return err
	}
	f, err := os.Create(filepath.Join(outDir, "tables.rs"))
	if err != nil {
		return err
	}
	w := bufio.NewWriter(f)
	fmt.Fprintf(w, "//! The ziggurat tables `ke`, `we` and `fe` of Go %s's `math/rand/v2/exp.go`, as written by\n", strings.TrimPrefix(runtime.Version(), "go"))
	fmt.Fprintf(w, "//! `go run ./tools/gocompat-vectors expfloat-tables <this directory>`; do not edit.\n\n")
	row := func(sep, name string, hex func(i int) string) {
		fmt.Fprintf(w, "%s#[rustfmt::skip]\npub(super) const %s: [u32; 256] = [\n", sep, name)
		for i := 0; i < 256; i += 8 {
			fmt.Fprint(w, "   ")
			for j := i; j < i+8; j++ {
				fmt.Fprintf(w, " %s,", hex(j))
			}
			fmt.Fprintln(w)
		}
		fmt.Fprintf(w, "];\n")
	}
	row("", "KE", func(i int) string { return hexU32(t.ke[i]) })
	row("\n", "WE", func(i int) string { return hexU32(math.Float32bits(t.we[i])) })
	row("\n", "FE", func(i int) string { return hexU32(math.Float32bits(t.fe[i])) })
	if err := w.Flush(); err != nil {
		f.Close()
		return err
	}
	return f.Close()
}

func writeExpAMD64(outDir string, wantFMA bool) error {
	if runtime.GOARCH != "amd64" {
		return fmt.Errorf("needs an amd64 host, running on %s", runtime.GOARCH)
	}
	avx, fma := detectAVXFMA()
	if !avx || !fma {
		return fmt.Errorf("the CPU must have AVX and FMA, found avx=%t fma=%t", avx, fma)
	}
	if info, ok := debug.ReadBuildInfo(); ok {
		for _, s := range info.Settings {
			if s.Key == "GOAMD64" && s.Value != "v1" {
				return fmt.Errorf("GOAMD64=%s: the Rust port covers Go's default v1 code generation", s.Value)
			}
		}
	}
	for _, p := range expProbes {
		got := math.Float64bits(math.Exp(math.Float64frombits(p.in)))
		want := p.plain
		if wantFMA {
			want = p.fma
		}
		if got != want {
			return fmt.Errorf("math.Exp(%#016x) = %#016x, want %#016x: Go is not running the %s branch of archExp (GODEBUG=%q)",
				p.in, got, want, branchName(wantFMA), os.Getenv("GODEBUG"))
		}
	}
	t, err := readExpTables()
	if err != nil {
		return err
	}
	for _, p := range branchProbes {
		want := 3
		if p.fmaAccepts == wantFMA {
			want = 2
		}
		if got := len(callExp(t, p.script, rand.NewPCG(expSeed, 3)).draws); got != want {
			return fmt.Errorf("script %#016x took %d draws, want %d: Go is not running the %s branch of archExp (GODEBUG=%q)",
				p.script, got, want, branchName(wantFMA), os.Getenv("GODEBUG"))
		}
	}
	calls, err := expCalls()
	if err != nil {
		return err
	}
	file := "expfloat-amd64-fma.txt"
	header := []string{"cpu: avx fma", "branch: archExp fma"}
	if !wantFMA {
		file = "expfloat-amd64-nofma.txt"
		header = []string{"cpu: avx fma", "branch: archExp nofma"}
		var rejecting []expCall
		for _, c := range calls {
			if c.rejects {
				rejecting = append(rejecting, c)
			}
		}
		calls = rejecting
	}
	return writeExpFile(filepath.Join(outDir, file), header, calls)
}

func writeExpARM64(outDir string) error {
	if runtime.GOARCH == "arm64" {
		calls, err := expCalls()
		if err != nil {
			return err
		}
		return writeExpFile(filepath.Join(outDir, "expfloat-arm64.txt"), nil, calls)
	}
	tmp, err := os.MkdirTemp("", "gocompat-expfloat-arm64-*")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)
	bin := filepath.Join(tmp, "gocompat-vectors")
	build := exec.Command("go", "build", "-o", bin, "./tools/gocompat-vectors")
	build.Env = append(os.Environ(), "GOOS=linux", "GOARCH=arm64", "CGO_ENABLED=0")
	build.Stderr = os.Stderr
	if err := build.Run(); err != nil {
		return fmt.Errorf("cross-building for arm64: %w", err)
	}
	abs, err := filepath.Abs(outDir)
	if err != nil {
		return err
	}
	goroot, err := exec.Command("go", "env", "GOROOT").Output()
	if err != nil {
		return err
	}
	run := exec.Command("qemu-aarch64", bin, "expfloat-arm64", abs)
	run.Env = append(os.Environ(), "GOROOT="+strings.TrimSpace(string(goroot)))
	run.Stderr = os.Stderr
	if err := run.Run(); err != nil {
		return fmt.Errorf("running the arm64 build under qemu-aarch64: %w", err)
	}
	return nil
}

// expCall is one call of ExpFloat64: the draws it consumed, in order, and its result.
// rejects is set when the call took the rejection path, where Go evaluates math.Exp.
type expCall struct {
	draws   []uint64
	result  float64
	rejects bool
}

// expSource serves a script of draws and then a PCG stream, recording what it served.
type expSource struct {
	script []uint64
	pcg    *rand.PCG
	used   []uint64
}

func (s *expSource) Uint64() uint64 {
	var u uint64
	if len(s.used) < len(s.script) {
		u = s.script[len(s.used)]
	} else {
		u = s.pcg.Uint64()
	}
	s.used = append(s.used, u)
	return u
}

// jAtLeast returns a draw of the 32-bit field not below ke, which Go rejects or sends
// to the tail.
func jAtLeast(p *rand.PCG, ke uint32) uint32 {
	return uint32(uint64(ke) + (p.Uint64()>>32)%(1<<32-uint64(ke)))
}

func expDraw(i uint8, j uint32) uint64 {
	return uint64(i)<<32 | uint64(j)
}

// callExp runs ExpFloat64 once on the script followed by the PCG stream. A call is on
// the rejection path when its first draw is not accepted at once and is not the tail.
func callExp(t *expTables, script []uint64, pcg *rand.PCG) expCall {
	src := &expSource{script: script, pcg: pcg}
	r := rand.New(src)
	v := r.ExpFloat64()
	first := src.used[0]
	j, i := uint32(first), uint8(first>>32)
	return expCall{draws: src.used, result: v, rejects: j >= t.ke[i] && i != 0}
}

// expCalls builds the vectors. Plain sequential calls from a PCG are the bulk. The
// forced calls cover every table row on the rejection path, and the tail, on the
// boundary draws of the 32-bit and 53-bit fields. Each row also has a scripted call
// that its rejection test rejects three times in a row, and the scripts of
// branchProbes tell the two branches of Go's amd64 math.Exp apart. checkExpRows fails
// when a row lacks a call accepted after one test, a call redrawn after a rejection or
// a call rejected three times in a row.
func expCalls() ([]expCall, error) {
	t, err := readExpTables()
	if err != nil {
		return nil, err
	}
	var calls []expCall
	pcg := rand.NewPCG(expSeed, 1)
	for range 8000 {
		calls = append(calls, callExp(t, nil, pcg))
	}

	forcedPCG := rand.NewPCG(expSeed, 2)
	floatDraws := []uint64{0, 1, 1 << 52, 1<<53 - 1, 1<<53 + 5, ^uint64(0), 0x8000000000000000, 0x7ff0000000000000}
	for i := range 256 {
		ke := t.ke[i]
		for _, j := range []uint32{0, ke - 1, ke, ke + 1, math.MaxUint32, math.MaxUint32 - 1} {
			calls = append(calls, callExp(t, []uint64{expDraw(uint8(i), j)}, forcedPCG))
		}
		for _, f := range floatDraws {
			calls = append(calls, callExp(t, []uint64{expDraw(uint8(i), math.MaxUint32), f}, forcedPCG))
			calls = append(calls, callExp(t, []uint64{expDraw(uint8(i), ke), f}, forcedPCG))
		}
		for range 12 {
			j := jAtLeast(forcedPCG, ke)
			calls = append(calls, callExp(t, []uint64{expDraw(uint8(i), j)}, forcedPCG))
		}
	}

	var tail int
	for i := 1; i < 256; i++ {
		for range 40 {
			j := jAtLeast(forcedPCG, t.ke[i])
			calls = append(calls, callExp(t, []uint64{expDraw(uint8(i), j)}, forcedPCG))
		}
	}
	for range 400 {
		i := uint8(1 + (forcedPCG.Uint64()>>32)%255)
		j := jAtLeast(forcedPCG, t.ke[i])
		var script []uint64
		for range 3 {
			script = append(script, expDraw(i, j), forcedPCG.Uint64())
		}
		calls = append(calls, callExp(t, script, forcedPCG))
	}
	// A 53-bit draw of 2^53-1 puts the bound at fe[i-1], the top of the row, and j of
	// 2^32-1 puts float32(Exp(-x)) at the bottom of it, so the test rejects. The call
	// ends on a draw of j = 0, which takes the fast path of every row but 1, where ke is
	// 0: three rejections and seven draws, or eight with a float draw of 0 that accepts.
	for i := 1; i < 256; i++ {
		var script []uint64
		for range 3 {
			script = append(script, expDraw(uint8(i), math.MaxUint32), 1<<53-1)
		}
		script = append(script, expDraw(uint8(i), 0))
		if t.ke[i] == 0 {
			script = append(script, 0)
		}
		c := callExp(t, script, forcedPCG)
		if len(c.draws) != len(script) {
			return nil, fmt.Errorf("row %d: the scripted chain of three rejections used %d draws, want %d", i, len(c.draws), len(script))
		}
		calls = append(calls, c)
	}

	for _, j := range []uint32{t.ke[0], t.ke[0] + 1, t.ke[0] + 2, math.MaxUint32, math.MaxUint32 - 1} {
		for _, f := range floatDraws {
			calls = append(calls, callExp(t, []uint64{expDraw(0, j), f}, forcedPCG))
		}
	}
	for _, f := range []uint64{0, 1, 2, 3, 1 << 20, 1 << 40, 1<<53 - 2, 1<<53 - 1, 1 << 52, 1<<52 + 1, 1<<53 - 1 + 1<<60} {
		calls = append(calls, callExp(t, []uint64{expDraw(0, math.MaxUint32), f}, forcedPCG))
	}
	for range 600 {
		j := jAtLeast(forcedPCG, t.ke[0])
		calls = append(calls, callExp(t, []uint64{expDraw(0, j), forcedPCG.Uint64()}, forcedPCG))
		tail++
	}

	probes := fusionProbes(t, forcedPCG)
	if len(probes) < 200 {
		return nil, fmt.Errorf("found %d float32 fusion probes, want at least 200", len(probes))
	}
	for _, script := range probes {
		calls = append(calls, callExp(t, script, forcedPCG))
	}
	for _, p := range branchProbes {
		calls = append(calls, callExp(t, p.script, forcedPCG))
	}

	if tail < 100 {
		return nil, fmt.Errorf("forced paths are too thin: %d tail calls", tail)
	}
	if err := checkExpRows(t, calls); err != nil {
		return nil, err
	}
	return calls, nil
}

// expTest is one rejection test of a call: the table row it drew and whether it
// rejected, which is when the call drew again.
type expTest struct {
	row      uint8
	rejected bool
}

// expTests returns the rejection tests a call ran, in order. A draw on the fast path or
// in the tail ends the call without one.
func expTests(t *expTables, draws []uint64) []expTest {
	var tests []expTest
	for pos := 0; pos < len(draws); {
		j, i := uint32(draws[pos]), uint8(draws[pos]>>32)
		if j < t.ke[i] || i == 0 {
			break
		}
		pos += 2
		tests = append(tests, expTest{row: i, rejected: pos < len(draws)})
	}
	return tests
}

// checkExpRows fails unless every table row from 1 to 255 has a call accepted by its
// first test, a call whose first test rejected and drew again, and a call with three
// rejections of that row in a row.
func checkExpRows(t *expTables, calls []expCall) error {
	var accepted, redrawn, chained [256]bool
	for _, c := range calls {
		tests := expTests(t, c.draws)
		if len(tests) == 0 {
			continue
		}
		first := tests[0]
		accepted[first.row] = accepted[first.row] || (len(tests) == 1 && !first.rejected)
		redrawn[first.row] = redrawn[first.row] || first.rejected
		run := 0
		for k, x := range tests {
			switch {
			case !x.rejected:
				run = 0
			case run > 0 && tests[k-1].row == x.row:
				run++
			default:
				run = 1
			}
			if run >= 3 {
				chained[x.row] = true
			}
		}
	}
	var missing []string
	for i := 1; i < 256; i++ {
		if !accepted[i] {
			missing = append(missing, fmt.Sprintf("row %d: no call accepted after one test", i))
		}
		if !redrawn[i] {
			missing = append(missing, fmt.Sprintf("row %d: no call rejected and redrawn", i))
		}
		if !chained[i] {
			missing = append(missing, fmt.Sprintf("row %d: no call rejected three times in a row", i))
		}
	}
	if len(missing) > 0 {
		return fmt.Errorf("forced paths miss %d cases, first %q", len(missing), missing[:min(len(missing), 5)])
	}
	return nil
}

// branchProbe is a script that the two branches of Go's amd64 math.Exp answer
// differently. The first draw is a rejection test of its row, and the float draw puts
// the test's bound between float32(Exp(-x)) of the two branches, so the branch that
// returns the larger value accepts after 2 draws and the other rejects and takes a
// third draw, the fast path of row 0, which returns 0 after 3.
type branchProbe struct {
	script     []uint64
	fmaAccepts bool
}

// branchProbes were found by evaluating the Rust ports of both branches, which the
// vectors pin to Go, on every draw j from ke[i] to 2^32-1 of every row i from 1 to 255,
// and keeping the pairs (i, j) whose float32(Exp(-x)) differs between the branches.
// Their float64 results lie within four ulps of a float32 rounding midpoint, and only
// four pairs qualify: rows 5, 29, 141 and 147. For each, the scripts take float draws m
// whose float32(m/2^53) is the lowest, the middle and the highest value that puts the
// bound fe[i] + f*(fe[i-1]-fe[i]) between the two float32 results, and the second
// script of row 5 was found by hand. writeExpAMD64 has Go confirm each script, and the
// scripts are in every vector file, where the Rust tests fail a wrong branch.
var branchProbes = []branchProbe{
	{[]uint64{expDraw(5, 0xf1c25b19), 0x000d854d10000000, 0}, true},
	{[]uint64{expDraw(5, 0xf1c25b19), 0x000d854f90000000, 0}, true},
	{[]uint64{expDraw(5, 0xf1c25b19), 0x000d855020000000, 0}, true},
	{[]uint64{expDraw(5, 0xf1c25b19), 0x000d855320000000, 0}, true},
	{[]uint64{expDraw(29, 0xfc36d22b), 0x0015806800000000, 0}, true},
	{[]uint64{expDraw(29, 0xfc36d22b), 0x00158070e0000000, 0}, true},
	{[]uint64{expDraw(29, 0xfc36d22b), 0x00158079a0000000, 0}, true},
	{[]uint64{expDraw(141, 0xfec224a3), 0x001538a440000000, 0}, false},
	{[]uint64{expDraw(141, 0xfec224a3), 0x001538ab80000000, 0}, false},
	{[]uint64{expDraw(141, 0xfec224a3), 0x001538b2a0000000, 0}, false},
	{[]uint64{expDraw(147, 0xfe3ce5c3), 0x001e576900000000, 0}, false},
	{[]uint64{expDraw(147, 0xfe3ce5c3), 0x001e577080000000, 0}, false},
	{[]uint64{expDraw(147, 0xfe3ce5c3), 0x001e577800000000, 0}, false},
}

func writeExpFile(path string, header []string, calls []expCall) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	w := bufio.NewWriter(f)
	fmt.Fprintf(w, "# gocompat ExpFloat64 vectors: expfloat, draw count, the draws, result bits\n")
	fmt.Fprintf(w, "# go: %s %s\n", strings.TrimPrefix(runtime.Version(), "go"), runtime.GOARCH)
	for _, h := range header {
		fmt.Fprintf(w, "# %s\n", h)
	}
	for _, c := range calls {
		var b strings.Builder
		b.WriteString("expfloat ")
		b.WriteString(strconv.Itoa(len(c.draws)))
		for _, d := range c.draws {
			fmt.Fprintf(&b, " %016x", d)
		}
		fmt.Fprintf(&b, " %016x", math.Float64bits(c.result))
		fmt.Fprintln(w, b.String())
	}
	if err := w.Flush(); err != nil {
		f.Close()
		return err
	}
	return f.Close()
}

// fusionProbes returns scripts whose rejection test of ExpFloat64 gives different
// outcomes when its float32 multiplication and addition round once and when they round
// twice. For a float draw whose two bounds fe[i] + f*(fe[i-1]-fe[i]) differ, the test
// differs when float32(Exp(-x)) equals the larger bound, so the draw of the 32-bit field
// is found by bisection on that non-increasing function. The candidates are found with
// float64 arithmetic; the vectors hold what Go returns.
func fusionProbes(t *expTables, pcg *rand.PCG) [][]uint64 {
	var probes [][]uint64
	for i := 1; i < 256; i++ {
		fe, step := t.fe[i], t.fe[i-1]-t.fe[i]
		bound := func(j uint32) float32 {
			return float32(math.Exp(-float64(j) * float64(t.we[i])))
		}
		found := 0
		for try := 0; try < 4000 && found < 6; try++ {
			m := pcg.Uint64() >> 11
			f := float32(float64(m) / (1 << 53))
			plain := fe + float32(f*step)
			fused := float32(math.FMA(float64(f), float64(step), float64(fe)))
			if plain == fused {
				continue
			}
			hi := max(plain, fused)
			lo, up := t.ke[i], uint32(math.MaxUint32)
			if bound(lo) < hi || bound(up) > hi {
				continue
			}
			for lo < up {
				mid := lo + (up-lo)/2
				if bound(mid) > hi {
					lo = mid + 1
				} else {
					up = mid
				}
			}
			if bound(lo) != hi {
				continue
			}
			probes = append(probes, []uint64{expDraw(uint8(i), lo), m})
			found++
		}
	}
	return probes
}
