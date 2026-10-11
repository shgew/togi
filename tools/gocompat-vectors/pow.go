package main

import (
	"bufio"
	"fmt"
	"math"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
)

// math.Pow is pure Go on amd64 and arm64, so its bits differ between targets only
// through the math.Exp and math.Log it calls. The pow-amd64-nofma file therefore holds
// only the pairs that reach math.Exp, which pow-amd64-fma and pow-arm64 repeat.
func init() {
	modes["pow-amd64-fma"] = func(outDir string) error { return writePowAMD64(outDir, true) }
	modes["pow-amd64-nofma"] = func(outDir string) error { return writePowAMD64(outDir, false) }
	modes["pow-arm64"] = writePowARM64
}

type powPair struct{ x, y float64 }

func writePowAMD64(outDir string, wantFMA bool) error {
	if err := checkAMD64Branch(wantFMA); err != nil {
		return err
	}
	pairs := powInputs()
	file := "pow-amd64-fma.txt"
	header := []string{"cpu: avx fma", "branch: archExp fma"}
	if !wantFMA {
		pairs = reachesExp(pairs)
		file = "pow-amd64-nofma.txt"
		header = []string{"cpu: avx fma", "branch: archExp nofma"}
	}
	return writePowFile(filepath.Join(outDir, file), header, pairs)
}

func checkAMD64Branch(wantFMA bool) error {
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
	return nil
}

func writePowARM64(outDir string) error {
	if runtime.GOARCH == "arm64" {
		return writePowFile(filepath.Join(outDir, "pow-arm64.txt"), nil, powInputs())
	}
	tmp, err := os.MkdirTemp("", "gocompat-pow-arm64-*")
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
	run := exec.Command("qemu-aarch64", bin, "pow-arm64", abs)
	run.Stderr = os.Stderr
	if err := run.Run(); err != nil {
		return fmt.Errorf("running the arm64 build under qemu-aarch64: %w", err)
	}
	return nil
}

func writePowFile(path string, header []string, pairs []powPair) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	w := bufio.NewWriter(f)
	fmt.Fprintf(w, "# gocompat pow vectors: function, base bits, exponent bits, output bits\n")
	fmt.Fprintf(w, "# go: %s %s\n", strings.TrimPrefix(runtime.Version(), "go"), runtime.GOARCH)
	for _, h := range header {
		fmt.Fprintf(w, "# %s\n", h)
	}
	for _, p := range pairs {
		fmt.Fprintf(w, "pow %016x %016x %016x\n", math.Float64bits(p.x), math.Float64bits(p.y), math.Float64bits(math.Pow(p.x, p.y)))
	}
	if err := w.Flush(); err != nil {
		f.Close()
		return err
	}
	return f.Close()
}

// reachesExp keeps the pairs for which math.Pow calls math.Exp and math.Log: a finite
// exponent with a fractional part other than one half, and a positive finite base.
func reachesExp(pairs []powPair) []powPair {
	var out []powPair
	for _, p := range pairs {
		_, yf := math.Modf(math.Abs(p.y))
		if yf == 0 || math.IsNaN(yf) || math.IsInf(p.y, 0) || math.Abs(p.y) == 0.5 {
			continue
		}
		if p.x > 0 && !math.IsInf(p.x, 0) && p.y != 1 && p.x != 1 {
			out = append(out, p)
		}
	}
	return out
}

type pairSet struct {
	seen  map[[2]uint64]bool
	pairs []powPair
}

func (s *pairSet) add(x, y float64) {
	if s.seen == nil {
		s.seen = map[[2]uint64]bool{}
	}
	key := [2]uint64{math.Float64bits(x), math.Float64bits(y)}
	if !s.seen[key] {
		s.seen[key] = true
		s.pairs = append(s.pairs, powPair{x, y})
	}
}

func (s *pairSet) cross(xs, ys []float64) {
	for _, x := range xs {
		for _, y := range ys {
			s.add(x, y)
		}
	}
}

func intRange(lo, hi int) []float64 {
	var out []float64
	for i := lo; i <= hi; i++ {
		out = append(out, float64(i))
	}
	return out
}

func neighbours(x float64, ulps int) []float64 {
	var s inputSet
	s.around(x, ulps)
	return s.xs
}

// The Go program calls math.Pow(Growth, d-1) in internal/sim/hazard.go with Growth 4
// by default and the fit tool's range [1.05, 8], math.Pow(Growth, -delta) with delta
// in [-50, 50] in tools/fit/fit.go, and math.Pow(1-rate, n-1) in
// internal/tuner/rerun_carried_replay_test.go with rate in (0, 1).
func powInputs() []powPair {
	var s pairSet
	nan := math.NaN()

	for _, v := range goVF {
		s.add(10, v)
	}
	for _, p := range goVFPowSC() {
		s.add(p.x, p.y)
	}

	growth := []float64{1.05, 1.5, 2, 3, 4, 5, 6, 7, 8, math.Nextafter(1.05, 2), math.Nextafter(8, 0), 1.0000001, 1.1, 1.25}
	var grid inputSet
	grid.grid(1.05, 8, 100)
	grid.uniform(newRand(10), 1.05, 8, 150)
	grid.add(growth...)
	s.cross(grid.xs, intRange(-60, 60))

	var rates inputSet
	rates.grid(1e-9, 0.999, 40)
	rates.uniform(newRand(11), 1e-9, 1, 80)
	var miss []float64
	for _, r := range rates.xs {
		miss = append(miss, 1-r)
	}
	s.cross(miss, intRange(0, 40))
	s.cross(miss, []float64{99, 100, 255, 1000, 4095, 4096, 4097, 100000})

	wide := []float64{1.05, 2, 3, 4, 8, 0.5, 0.1, 10, 0.9999999}
	s.cross(wide, intRange(-1100, 1100))
	s.cross([]float64{1.0000001, 0.25, 1e-300, 1e300, 5e-324, math.MaxFloat64, -2, -3, -0.5, -1.5, -10, -1e-300}, intRange(-70, 70))

	var big inputSet
	big.logUniform(newRand(12), -1074, 1023, 100, true)
	s.cross(big.xs, []float64{2, 3, 4, 5, 7, 63, 64, 65, 255, 256, 1000, 4095, 4096, 4097, 8191, 8192, -2, -3, -4096, -4097, -8192, 1 << 20, -(1 << 20)})

	var fracX inputSet
	fracX.logUniform(newRand(13), -1074, 1023, 120, false)
	fracX.logUniform(newRand(14), -20, 20, 120, false)
	fracX.grid(0, 8, 60)
	fracX.add(growth...)
	fracX.add(math.E, math.Pi, math.Sqrt2, 10, 0.1, 1e-12, 1e6)
	var fracY inputSet
	fracY.uniform(newRand(15), -20, 20, 40)
	fracY.uniform(newRand(16), -2, 2, 20)
	fracY.add(0.25, -0.25, 0.75, -0.75, 1.5, -1.5, 2.5, -2.5, 0.1, 0.9, 1.0/3, 2.0/3, 0.499999999, 0.500000001, 1e-9, -1e-9, 1e-300, 3.7, 12.34)
	s.cross(fracX.xs, fracY.xs)

	for _, k := range []float64{0, 1, 2, 3, 10, 100, 1000} {
		for _, d := range neighbours(0.5, 3) {
			s.cross([]float64{1.05, 2, 4, 8, 0.3, 17, 1e-5, 1e5, 1e-300, 1e300, 5e-324, math.MaxFloat64}, []float64{k + d, -(k + d)})
		}
	}
	for _, d := range neighbours(0.5, 4) {
		s.add(2, d)
		s.add(2, -d)
		s.add(0.37, d)
	}
	for _, d := range neighbours(1, 4) {
		s.cross([]float64{1.05, 2, 4, 8, 0.3, 17, -2}, []float64{d, -d})
	}

	var odd inputSet
	odd.add(1<<53-1, 1<<53, 1<<53+2, 1<<62, 1<<63, 1<<64, 1e19, 1e300, math.MaxFloat64, 9007199254740991, 9007199254740993, 9223372036854774784, 9223372036854775808)
	var oddY []float64
	for _, v := range odd.xs {
		oddY = append(oddY, v, -v)
	}
	s.cross([]float64{math.Copysign(0, -1), 0, -2, 2, -1, 1, 0.5, -0.5, math.Nextafter(1, 2), math.Nextafter(1, 0), math.Nextafter(-1, -2), math.Nextafter(-1, 0), math.Inf(1), math.Inf(-1), nan}, oddY)

	specialsX := []float64{0, math.Copysign(0, -1), 1, -1, 2, -2, 0.5, -0.5, 4, -4, math.Pi, -math.Pi, math.Inf(1), math.Inf(-1), nan, 5e-324, -5e-324, math.MaxFloat64, -math.MaxFloat64}
	specialsY := []float64{0, math.Copysign(0, -1), 1, -1, 0.5, -0.5, 2, -2, 3, -3, 4, -4, math.Pi, -math.Pi, math.Inf(1), math.Inf(-1), nan, 5e-324, -5e-324, math.MaxFloat64, -math.MaxFloat64, 1e19, -1e19, 1 << 53, 1<<53 + 2, 1<<53 - 1, -(1<<53 - 1), 1<<31 + 1, 1 << 32, 1 << 45, 1 << 63, -(1 << 63)}
	s.cross(specialsX, specialsY)

	subnormal := []float64{0x1p-1022, math.Nextafter(0x1p-1022, 0), 0x0.8p-1022, 3e-310, 5e-324, 0x1p-1074 * 3, math.Float64frombits(0x000fffffffffffff)}
	s.cross(subnormal, append(intRange(-12, 12), 0.5, -0.5, 1.5, -1.5, 0.3, -0.3, 1.0/3, 100.25, -100.75))
	return s.pairs
}

// goVFPowSC is vfpowSC from Go's src/math/all_test.go (lines 1653-1735), copied
// verbatim; Go's TestPow compares each result with powSC (lines 1739-1819).
func goVFPowSC() []powPair {
	inf, ninf, nan, nz := math.Inf(1), math.Inf(-1), math.NaN(), math.Copysign(0, -1)
	pi := math.Pi
	return []powPair{
		{ninf, -pi},
		{ninf, -3},
		{ninf, nz},
		{ninf, 0},
		{ninf, 1},
		{ninf, 3},
		{ninf, pi},
		{ninf, 0.5},
		{ninf, nan},

		{-pi, ninf},
		{-pi, -pi},
		{-pi, nz},
		{-pi, 0},
		{-pi, 1},
		{-pi, pi},
		{-pi, inf},
		{-pi, nan},

		{-1, ninf},
		{-1, inf},
		{-1, nan},
		{-0.5, ninf},
		{-0.5, inf},
		{nz, ninf},
		{nz, -pi},
		{nz, -0.5},
		{nz, -3},
		{nz, 3},
		{nz, pi},
		{nz, 0.5},
		{nz, inf},

		{0, ninf},
		{0, -pi},
		{0, -3},
		{0, nz},
		{0, 0},
		{0, 3},
		{0, pi},
		{0, inf},
		{0, nan},

		{0.5, ninf},
		{0.5, inf},
		{1, ninf},
		{1, inf},
		{1, nan},

		{pi, ninf},
		{pi, nz},
		{pi, 0},
		{pi, 1},
		{pi, inf},
		{pi, nan},
		{inf, -pi},
		{inf, nz},
		{inf, 0},
		{inf, 1},
		{inf, pi},
		{inf, nan},
		{nan, -pi},
		{nan, nz},
		{nan, 0},
		{nan, 1},
		{nan, pi},
		{nan, nan},

		{2, float64(1 << 32)},
		{2, -float64(1 << 32)},
		{-2, float64(1<<32 + 1)},
		{0.5, float64(1 << 45)},
		{0.5, -float64(1 << 45)},
		{math.Nextafter(1, 2), float64(1 << 63)},
		{math.Nextafter(1, -2), float64(1 << 63)},
		{math.Nextafter(-1, 2), float64(1 << 63)},
		{math.Nextafter(-1, -2), float64(1 << 63)},

		{nz, 1e19},
		{nz, -1e19},
		{nz, 1<<53 - 1},
		{nz, -(1<<53 - 1)},
	}
}
