package mprime

import (
	"fmt"
	"os"
	"path/filepath"
	"regexp"

	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/machine"
)

type Mprime struct {
	pkg string
}

func New(pkg string) *Mprime { return &Mprime{pkg: pkg} }

func (m *Mprime) Name() string { return "mprime" }

func (m *Mprime) Check() (string, error) {
	bin := filepath.Join(m.pkg, "bin/mprime")
	if err := backend.CheckExecutable("mprime", bin); err != nil {
		return "", err
	}
	return bin, nil
}

func (m *Mprime) Prepare(w machine.Workload, dir string, cpus []int) (backend.Launch, error) {
	var minFFT, maxFFT int
	var avx, fma, avx2, avx512 int
	switch w.Base {
	case "mprime-sse-4k-21k":
		minFFT, maxFFT = 4, 21
	case "mprime-avx2-36k-248k":
		minFFT, maxFFT = 36, 248
		avx, fma, avx2 = 1, 1, 1
	case "mprime-avx512-36k-248k":
		minFFT, maxFFT = 36, 248
		avx, fma, avx2, avx512 = 1, 1, 1, 1
	default:
		return backend.Launch{}, fmt.Errorf("mprime: no configuration for workload %s", w.ID)
	}
	if _, err := m.Check(); err != nil {
		return backend.Launch{}, err
	}
	hyperthreading := 0
	if len(cpus) > 1 {
		hyperthreading = 1
	}
	settings := fmt.Sprintf("NumCPUs=1\nCoresPerTest=1\nMinTortureFFT=%d\nMaxTortureFFT=%d\nTortureTime=1\nTortureHyperthreading=%d\nTortureThreads=%d\nCpuSupportsAVX=%d\nCpuSupportsFMA3=%d\nCpuSupportsFMA4=0\nCpuSupportsAVX2=%d\nCpuSupportsAVX512F=%d\n", minFFT, maxFFT, hyperthreading, len(cpus), avx, fma, avx2, avx512)
	prime := "V30OptionsConverted=1\nStressTester=1\nUsePrimenet=0\n" + settings + "EnableSetAffinity=0\n"
	local := "ErrorCheck=1\nSumInputsErrorCheck=1\n" + settings
	for _, file := range []struct{ name, contents string }{{"prime.txt", prime}, {"local.txt", local}} {
		if err := os.WriteFile(filepath.Join(dir, file.name), []byte(file.contents), 0644); err != nil {
			return backend.Launch{}, fmt.Errorf("write %s: %w", filepath.Join(dir, file.name), err)
		}
	}
	return backend.Launch{Argv: []string{filepath.Join(m.pkg, "bin/mprime"), "-t", "-W" + dir}, Files: []string{"prime.txt", "local.txt"}, Watch: []string{"results.txt"}}, nil
}

var progress = regexp.MustCompile(`(?i)Self-test (\d+K?)(?: \(thread \d+ of \d+\))? passed`)
var computationErrors = []*regexp.Regexp{
	regexp.MustCompile(`(?i)FATAL ERROR|ERROR: ILLEGAL SUMOUT|Possible hardware failure|Hardware failure detected|Maximum number of warnings exceeded|TORTURE TEST FAILED|Torture Test completed .* - [1-9]\d* errors`),
	regexp.MustCompile(`(?i)ERROR: SUM\(INPUTS\) != SUM\(OUTPUTS\)|ERROR: Shift counter corrupt|ERROR: Illegal double encountered|ERROR: FFT data has been zeroed|ERROR: Jacobi error check failed|Warning: ILLEGAL SUMOUT|Warning: SUMOUT MISMATCH`),
}
var setupErrors = regexp.MustCompile(`(?i)Error allocating memory|Out of memory|Unable to allocate memory|Cannot initialize FFT code`)

func (m *Mprime) Classify(line string) backend.Line {
	for _, pattern := range computationErrors {
		if pattern.MatchString(line) {
			return backend.Line{Kind: backend.ComputationError}
		}
	}
	if setupErrors.MatchString(line) {
		return backend.Line{Kind: backend.SetupError}
	}
	if match := progress.FindStringSubmatch(line); match != nil {
		return backend.Line{Kind: backend.Progress, Progress: "self-test " + match[1] + " passed"}
	}
	return backend.Line{Kind: backend.Other}
}
