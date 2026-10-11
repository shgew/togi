package main

import (
	"bufio"
	"errors"
	"fmt"
	"math"
	"math/rand/v2"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"runtime/debug"
	"strings"
)

const mathSeed = 698

// The amd64 modes run on the machine's real CPU; only the second one is started
// with GODEBUG=cpu.fma=off. math-arm64 cross-builds this program and runs it
// under qemu-aarch64 unless it already runs on arm64.
func init() {
	modes["math-amd64-fma"] = func(outDir string) error { return writeMathAMD64(outDir, true) }
	modes["math-amd64-nofma"] = func(outDir string) error { return writeMathAMD64(outDir, false) }
	modes["math-arm64"] = writeMathARM64
}

type mathFunc struct {
	name   string
	f      func(float64) float64
	inputs func() []float64
}

var mathFuncs = []mathFunc{
	{"exp", math.Exp, expInputs},
	{"expm1", math.Expm1, expm1Inputs},
	{"log", math.Log, logInputs},
	{"log1p", math.Log1p, log1pInputs},
}

// expProbes are inputs on which Go's archExp gives different bits with and
// without FMA, found by running both branches.
var expProbes = []struct{ in, fma, plain uint64 }{
	{0xc048fd604189374c, 0x3b6dc0075a415769, 0x3b6dc0075a41576a},
	{0xc041153f7ced9168, 0x3cda270f51115ae9, 0x3cda270f51115ae8},
	{0xc03180c49ba5e354, 0x3e5ae181a3cde3cf, 0x3e5ae181a3cde3ce},
	{0xbffa4fdf3b645a20, 0x3fc8b7cc7bf0803f, 0x3fc8b7cc7bf08040},
	{0x402bf16872b020c4, 0x4131d5addcdbdad3, 0x4131d5addcdbdad2},
	{0x403dffdf3b645a1c, 0x42a36dca3d0518c7, 0x42a36dca3d0518c8},
}

func writeMathAMD64(outDir string, wantFMA bool) error {
	if runtime.GOARCH != "amd64" {
		return fmt.Errorf("needs an amd64 host, running on %s", runtime.GOARCH)
	}
	avx, fma := detectAVXFMA()
	if !avx || !fma {
		return fmt.Errorf("the CPU must have AVX and FMA, found avx=%t fma=%t", avx, fma)
	}
	if err := requireGOAMD64v1(); err != nil {
		return err
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
	funcs := mathFuncs
	file := "math-amd64-fma.txt"
	if !wantFMA {
		funcs = mathFuncs[:1]
		file = "math-amd64-nofma.txt"
	}
	header := []string{
		"cpu: avx fma",
		"branch: archExp " + branchName(wantFMA),
	}
	return writeMathFile(filepath.Join(outDir, file), header, funcs)
}

func branchName(fma bool) string {
	if fma {
		return "fma"
	}
	return "nofma"
}

func requireGOAMD64v1() error {
	info, ok := debug.ReadBuildInfo()
	if !ok {
		return errors.New("no build info, cannot read GOAMD64")
	}
	for _, s := range info.Settings {
		if s.Key == "GOAMD64" && s.Value != "v1" {
			return fmt.Errorf("GOAMD64=%s: the Rust port covers Go's default v1 code generation", s.Value)
		}
	}
	return nil
}

func writeMathARM64(outDir string) error {
	if runtime.GOARCH == "arm64" {
		return writeMathFile(filepath.Join(outDir, "math-arm64.txt"), nil, mathFuncs)
	}
	tmp, err := os.MkdirTemp("", "gocompat-arm64-*")
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
	run := exec.Command("qemu-aarch64", bin, "math-arm64", abs)
	run.Stderr = os.Stderr
	if err := run.Run(); err != nil {
		return fmt.Errorf("running the arm64 build under qemu-aarch64: %w", err)
	}
	return nil
}

func writeMathFile(path string, header []string, funcs []mathFunc) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	w := bufio.NewWriter(f)
	fmt.Fprintf(w, "# gocompat math vectors: function, input bits, output bits\n")
	fmt.Fprintf(w, "# go: %s %s\n", strings.TrimPrefix(runtime.Version(), "go"), runtime.GOARCH)
	for _, h := range header {
		fmt.Fprintf(w, "# %s\n", h)
	}
	for _, fn := range funcs {
		for _, x := range fn.inputs() {
			fmt.Fprintf(w, "%s %016x %016x\n", fn.name, math.Float64bits(x), math.Float64bits(fn.f(x)))
		}
	}
	if err := w.Flush(); err != nil {
		f.Close()
		return err
	}
	return f.Close()
}

type inputSet struct {
	seen map[uint64]bool
	xs   []float64
}

func (s *inputSet) add(xs ...float64) {
	if s.seen == nil {
		s.seen = map[uint64]bool{}
	}
	for _, x := range xs {
		b := math.Float64bits(x)
		if !s.seen[b] {
			s.seen[b] = true
			s.xs = append(s.xs, x)
		}
	}
}

func (s *inputSet) around(x float64, ulps int) {
	s.add(x)
	lo, hi := x, x
	for range ulps {
		lo = math.Nextafter(lo, math.Inf(-1))
		hi = math.Nextafter(hi, math.Inf(1))
		s.add(lo, hi)
	}
}

func (s *inputSet) grid(lo, hi float64, n int) {
	for i := 0; i <= n; i++ {
		s.add(lo + float64(i)*((hi-lo)/float64(n)))
	}
}

// logUniform adds n magnitudes spread evenly over the binary exponents lo to
// hi, with random mantissas, in both signs when signed is set.
func (s *inputSet) logUniform(r *rand.Rand, lo, hi, n int, signed bool) {
	for range n {
		m := 1 + float64(r.Uint64()>>11)/(1<<53)
		x := math.Ldexp(m, lo+r.IntN(hi-lo+1))
		s.add(x)
		if signed {
			s.add(-x)
		}
	}
}

func (s *inputSet) uniform(r *rand.Rand, lo, hi float64, n int) {
	for range n {
		s.add(lo + float64(r.Uint64()>>11)/(1<<53)*(hi-lo))
	}
}

func (s *inputSet) specials() {
	nan := math.Float64frombits(0x7ff8000000000001)
	s.add(0, math.Copysign(0, -1), nan, -nan, math.Float64frombits(0x7ff0000000000001),
		math.Float64frombits(0xfff8000000000000), math.Inf(1), math.Inf(-1))
	s.around(1, 4)
	s.around(-1, 4)
	s.around(2, 2)
	s.around(0.5, 2)
	s.around(math.SmallestNonzeroFloat64, 3)
	s.around(-math.SmallestNonzeroFloat64, 3)
	s.around(0x1p-1022, 3)
	s.around(-0x1p-1022, 3)
	s.around(math.MaxFloat64, 3)
	s.around(-math.MaxFloat64, 3)
	s.add(math.Float64frombits(0x000fffffffffffff), -math.Float64frombits(0x000fffffffffffff))
}

func (s *inputSet) powersOfTwo(lo, hi, ulps int) {
	for e := lo; e <= hi; e++ {
		s.around(math.Ldexp(1, e), ulps)
	}
}

func newRand(stream uint64) *rand.Rand {
	return rand.New(rand.NewPCG(mathSeed, stream))
}

// goVF is vf from Go's src/math/all_test.go (lines 14-25), copied verbatim.
var goVF = []float64{
	4.9790119248836735e+00,
	7.7388724745781045e+00,
	-2.7688005719200159e-01,
	-5.0106036182710749e+00,
	9.6362937071984173e+00,
	2.9263772392439646e+00,
	5.2290834314593066e+00,
	2.7279399104360102e+00,
	1.8253080916808550e+00,
	-8.6859247685756013e+00,
}

// goTestExp adds the inputs of Go's TestExp (all_test.go lines 2438-2454):
// vf[i] and vfexpSC (lines 1058-1072).
func (s *inputSet) goTestExp() {
	s.add(goVF...)
	s.add(math.Inf(-1), -2000, 2000, math.Inf(1), math.NaN(),
		7.097827128933841e+02, // smallest float64 that overflows Exp(x)
		1.48852223e+09,        // Issue 18912
		1.4885222e+09,
		1,
		3.725290298461915e-09, // near zero
		-740,                  // denormal
	)
}

// goTestExpm1 adds the inputs of Go's TestExpm1 (all_test.go lines 2456-2474):
// vf[i]/100, vf[i]*10 and vfexpm1SC (lines 1113-1121).
func (s *inputSet) goTestExpm1() {
	for _, v := range goVF {
		s.add(v / 100)
	}
	for _, v := range goVF {
		s.add(v * 10)
	}
	s.add(math.Inf(-1), -710, math.Copysign(0, -1), 0, 710, math.Inf(1), math.NaN())
}

// goTestLog adds the inputs of Go's TestLog (all_test.go lines 2771-2786):
// Abs(vf[i]), 10 and vflogSC (lines 1537-1545).
func (s *inputSet) goTestLog() {
	for _, v := range goVF {
		s.add(math.Abs(v))
	}
	s.add(10)
	s.add(math.Inf(-1), -math.Pi, math.Copysign(0, -1), 0, 1, math.Inf(1), math.NaN())
}

// goTestLog1p adds the inputs of Go's TestLog1p (all_test.go lines 2823-2839):
// vf[i]/100, 9 and the first len(vflogSC) = 7 entries of vflog1pSC (lines
// 1569-1578), which is all the Go test iterates; its eighth entry,
// 4503599627370496.5, is never run by Go and is left out.
func (s *inputSet) goTestLog1p() {
	for _, v := range goVF {
		s.add(v / 100)
	}
	s.add(9)
	s.add(math.Inf(-1), -math.Pi, -1, math.Copysign(0, -1), 0, math.Inf(1), math.NaN())
}

// The ranges below come from the Go code that calls these functions:
// Exp(LogRate + Effect + Slope*(depth/size-25)) in internal/sim/hazard.go and
// Exp(-|x|) in internal/sim/shared_voltage.go; Expm1(-hazard) in
// internal/sim/hazard.go; Log(miss) and Log1p(-rate) with miss and rate in
// (0, 1) in internal/config/config.go; the fit tool's Log(p) and Log1p(-p) for
// probabilities p in (0, 1), Log of positive shares and bounds, and
// Exp(Log(low)..Log(high)) searches over [1e-12, 1e6].
func expInputs() []float64 {
	var s inputSet
	s.specials()
	for _, t := range []float64{
		709.78271289338397, 7.09782712893384e+02, -745.13321910194111, -745.13321910194122,
		-708.3964185322641, -744.4400719213812, 709, -709, 710, -746,
	} {
		s.around(t, 8)
	}
	for n := -1080; n <= 1030; n++ {
		s.around((float64(n)+0.5)*math.Ln2, 1)
	}
	for _, t := range []float64{0x1p-28, 0x1p-29, 0x1p-54, 0x1p-53, 0x1p-52, 0.5 * math.Ln2, 1.5 * math.Ln2, math.Ln2} {
		s.around(t, 3)
		s.around(-t, 3)
	}
	for _, x := range []float64{2147483647, 2147483648, 2147483649, 1.5e9, 3e9, 1e10, 1e100, 1e300} {
		s.add(-x, -x/math.Log2E, x)
	}
	s.around(-2147483648.5/math.Log2E, 6)
	s.around(-2147483647.5/math.Log2E, 6)
	s.around(-2147483648/math.Log2E, 6)
	s.grid(-40, 40, 4000)
	s.grid(-1, 1, 1000)
	s.grid(-745.2, 709.8, 1500)
	s.grid(-12, -3.5, 500)
	s.powersOfTwo(-1074, 10, 1)
	r := newRand(1)
	s.uniform(r, -745.2, 709.8, 1500)
	s.uniform(r, -30, 12, 1500)
	s.logUniform(r, -1074, 9, 1000, true)
	s.goTestExp()
	return s.xs
}

func expm1Inputs() []float64 {
	var s inputSet
	s.specials()
	for _, t := range []float64{
		7.09782712893383973096e+02, 3.88162421113569373274e+01, 1.03972077083991796413e+00,
		3.46573590279972654709e-01, 0x1p-54, 0x1p-53, 0x1p-55, -0.25, 0.25, 56 * math.Ln2, 20 * math.Ln2,
	} {
		s.around(t, 6)
		s.around(-t, 6)
	}
	for n := -60; n <= 60; n++ {
		s.around((float64(n)+0.5)*math.Ln2, 2)
		s.around(float64(n)*math.Ln2, 2)
		s.around((float64(n)+0.5)/math.Log2E, 2)
		s.around(float64(n), 1)
	}
	s.grid(-60, 0, 4000)
	s.grid(-1, 1, 1000)
	s.grid(0, 60, 1000)
	s.grid(-0.001, 0.001, 300)
	s.powersOfTwo(-1074, 9, 1)
	r := newRand(2)
	s.uniform(r, -45, 45, 1000)
	s.uniform(r, 45, 709.8, 300)
	s.logUniform(r, -1074, 9, 1000, true)
	s.goTestExpm1()
	return s.xs
}

func logInputs() []float64 {
	var s inputSet
	s.specials()
	for _, t := range []float64{math.Sqrt2 / 2, math.Sqrt2, 0.75, 1.5, math.E, 1e-12, 1e6, 0.006, 1 - 1e-4, 1e-4, 0.8, 0.03, 1e-5, math.Exp(-5), math.Exp(5)} {
		s.around(t, 3)
		s.around(-t, 1)
	}
	s.powersOfTwo(-1074, 1023, 1)
	for e := -1074; e <= 1023; e += 3 {
		s.around(math.Ldexp(math.Sqrt2/2, e), 1)
	}
	for _, e := range []int{-1022, -1023, -1030, -1050, -1073, -1074} {
		s.add(math.Ldexp(1.5, e), math.Ldexp(1.25, e), math.Ldexp(math.Sqrt2, e))
	}
	s.grid(0, 1, 4000)
	s.grid(0.9, 1.1, 1000)
	s.grid(1, 1000, 500)
	r := newRand(3)
	s.uniform(r, 0, 1, 1000)
	for range 1000 {
		s.add(math.Float64frombits(r.Uint64() >> 12))
	}
	s.logUniform(r, -1074, 1023, 1500, false)
	s.logUniform(r, -45, 25, 1500, false)
	s.goTestLog()
	return s.xs
}

func log1pInputs() []float64 {
	var s inputSet
	s.specials()
	for _, t := range []float64{
		4.142135623730950488017e-01, -2.928932188134524755992e-01, 0x1p-29, 0x1p-54, 0x1p53, 0x1p-20,
		math.Sqrt2 - 1, math.Sqrt2/2 - 1,
	} {
		s.around(t, 6)
		s.around(-t, 6)
	}
	for k := -60; k <= 64; k++ {
		p := math.Ldexp(1, k)
		s.around(p-1, 2)
		s.around(p*math.Sqrt2-1, 2)
		s.around(p*math.Sqrt2/2-1, 2)
	}
	for j := 1; j <= 53; j++ {
		s.around(-1+math.Ldexp(1, -j), 2)
		s.around(-math.Ldexp(1, -j), 2)
	}
	s.around(-1, 8)
	s.grid(-1, 0, 4000)
	s.grid(0, 1, 2000)
	s.grid(-0.001, 0.001, 300)
	s.grid(1, 1000, 500)
	s.powersOfTwo(-1074, 1023, 1)
	r := newRand(4)
	s.uniform(r, -1, 0, 1000)
	s.uniform(r, 0, 2, 500)
	s.logUniform(r, -1074, 1023, 1500, true)
	s.logUniform(r, -60, 60, 1000, false)
	s.goTestLog1p()
	return s.xs
}
