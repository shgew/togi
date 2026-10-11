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

const lgammaStream = 700

// Go's Lgamma runs the same pure-Go code on both targets, with Log from
// log_amd64.s on amd64 and log.go on arm64, and the arm64 compiler fuses
// multiplications. FMA does not matter on amd64, so there is one amd64 file.
func init() {
	modes["lgamma-amd64"] = writeLgammaAMD64
	modes["lgamma-arm64"] = writeLgammaARM64
}

func writeLgammaAMD64(outDir string) error {
	if runtime.GOARCH != "amd64" {
		return fmt.Errorf("needs an amd64 host, running on %s", runtime.GOARCH)
	}
	if err := requireGOAMD64v1(); err != nil {
		return err
	}
	return writeLgammaFile(filepath.Join(outDir, "lgamma-amd64.txt"))
}

func writeLgammaARM64(outDir string) error {
	if runtime.GOARCH == "arm64" {
		return writeLgammaFile(filepath.Join(outDir, "lgamma-arm64.txt"))
	}
	tmp, err := os.MkdirTemp("", "gocompat-lgamma-arm64-*")
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
	run := exec.Command("qemu-aarch64", bin, "lgamma-arm64", abs)
	run.Stderr = os.Stderr
	if err := run.Run(); err != nil {
		return fmt.Errorf("running the arm64 build under qemu-aarch64: %w", err)
	}
	return nil
}

func writeLgammaFile(path string) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	w := bufio.NewWriter(f)
	fmt.Fprintf(w, "# gocompat lgamma vectors: function, input bits, output bits, sign\n")
	fmt.Fprintf(w, "# go: %s %s\n", strings.TrimPrefix(runtime.Version(), "go"), runtime.GOARCH)
	for _, x := range lgammaInputs() {
		y, sign := math.Lgamma(x)
		fmt.Fprintf(w, "lgamma %016x %016x %d\n", math.Float64bits(x), math.Float64bits(y), sign)
	}
	if err := w.Flush(); err != nil {
		f.Close()
		return err
	}
	return f.Close()
}

// goTestLgamma adds the inputs of Go's TestLgamma (all_test.go lines 2758-2768):
// vf[i] and vflgammaSC (lines 1518-1526).
func (s *inputSet) goTestLgamma() {
	s.add(goVF...)
	s.add(math.Inf(-1), -3, 0, 1, 2, math.Inf(1), math.NaN())
}

// The call sites are tools/modelcheck's logChoose, which takes Lgamma of the
// integers k+1, n-k+1 and n+1 for every k in [0, n] and each trial count n of a
// group. lgammaInputs covers every integer up to 2^14+1, so every call for
// groups of up to 2^14 trials (including modelcheck's test with n=10000), the
// rest of the integers up to 2^20 coarsely, and the other branches around them.
func lgammaInputs() []float64 {
	var s inputSet
	s.specials()
	for k := 1; k <= 1<<14+1; k++ {
		s.add(float64(k))
	}
	for k := 1<<14 + 1; k <= 1<<20; k += 997 {
		s.add(float64(k))
	}
	for e := 10; e <= 64; e++ {
		s.around(math.Ldexp(1, e), 2)
		s.around(math.Ldexp(1, e)+1, 1)
	}

	const ymin = 1.461632144968362245
	const tc = 1.46163214496836224576e+00
	for _, t := range []float64{
		0x1p-70, 0x1p-71, 0x1p-69, 0x1p-52, 0x1p-53, 0.25, 0.9, 1, 2, 8, 0.5, 1.5,
		ymin - 1 + 0.27, ymin - 1 - 0.27, ymin + 0.27, ymin - 0.27, tc, tc - 1, ymin,
		0x1p52, 0x1p53, 0x1p58, 0x1p57, 0x1p59, 0x1p52 - 1, 0x1p52 + 1, 0x1p53 - 1,
		7, 6, 5, 4, 3, 7.5, 3.5, 4.5, 5.5, 6.5, 2.5, 8.5,
	} {
		s.around(t, 4)
		s.around(-t, 4)
	}
	for k := 0; k <= 16; k++ {
		for _, base := range []float64{0, 2, 4, 6, 8, 100, 1000, 1e6} {
			t := base + float64(k)/8
			s.around(t, 1)
			s.around(-t, 1)
		}
	}
	for k := range 130 {
		s.around(float64(k)+0.25, 1)
		s.around(-(float64(k) + 0.25), 1)
		s.around(float64(k)+0.75, 1)
		s.around(-(float64(k) + 0.75), 1)
	}
	for n := -60; n <= 60; n++ {
		s.around(float64(n), 1)
		s.around(float64(n)+0.5, 1)
	}
	for e := -1074; e <= -60; e += 7 {
		s.around(math.Ldexp(1, e), 1)
		s.around(-math.Ldexp(1, e), 1)
	}
	s.powersOfTwo(-1074, 70, 1)
	for e := -1074; e <= 70; e++ {
		s.add(-math.Ldexp(1, e))
	}

	s.grid(0, 1, 4000)
	s.grid(0, 3, 3000)
	s.grid(1.5, 2.5, 1000)
	s.grid(0, 10, 4000)
	s.grid(8, 100, 2000)
	s.grid(-1, 0, 2000)
	s.grid(-10, 0, 4000)
	s.grid(-60, -40, 1000)
	s.grid(-300, -250, 500)
	s.grid(-1e6, -1e6+50, 300)

	r := newRand(lgammaStream)
	s.uniform(r, 0, 1, 1500)
	s.uniform(r, 0, 10, 1500)
	s.uniform(r, 8, 1000, 1000)
	s.uniform(r, -10, 0, 1500)
	s.uniform(r, -1000, 0, 1000)
	s.logUniform(r, -80, 62, 2000, true)
	s.logUniform(r, -1074, 1023, 500, true)
	for range 500 {
		s.add(-float64(1 + r.IntN(1<<20)))
		s.add(float64(1 + r.IntN(1<<20)))
	}
	s.goTestLgamma()
	return s.xs
}
