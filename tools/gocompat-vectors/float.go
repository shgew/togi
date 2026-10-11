package main

import (
	"bufio"
	"bytes"
	"compress/gzip"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"math"
	"math/rand/v2"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
)

const (
	floatSeed         = 697
	wideSamples       = 20000
	decimalSamples    = 20000
	tieSamples        = 4000
	formatTies        = 800
	formatWide        = 800
	formatDecimal     = 1200
	fixedFormatMaxExp = 75
)

type formatSpec struct {
	fmt  byte
	prec int
}

// formatSpecs covers every strconv.FormatFloat call and fmt verb in the Go
// code that formats a float (docs/rust-migration.md lists each site), plus the
// neighbouring precisions and the upper-case verbs.
var formatSpecs = []formatSpec{
	{'f', -1}, {'f', 0}, {'f', 1}, {'f', 2}, {'f', 3}, {'f', 4}, {'f', 6}, {'f', 9},
	{'e', -1}, {'e', 0}, {'e', 3}, {'e', 6}, {'e', 17},
	{'E', -1}, {'E', 3},
	{'g', -1}, {'g', 0}, {'g', 1}, {'g', 3}, {'g', 6}, {'g', 9}, {'g', 17},
	{'G', -1}, {'G', 6},
}

var goldenSpecs = map[formatSpec]bool{{'f', 2}: true, {'f', 3}: true, {'g', 6}: true, {'g', 17}: true}

func init() {
	modes["float"] = writeFloatVectors
}

func writeFloatVectors(outDir string) error {
	golden, err := goldenFloats(".")
	if err != nil {
		return err
	}
	rng := rand.New(rand.NewPCG(floatSeed, floatSeed))
	fixed := fixedFloats()
	wide := wideFloats(rng, wideSamples)
	decimal := decimalFloats(rng, decimalSamples)
	ties := tieFloats(rng, tieSamples)

	jsonSet := dedupe(slices.Concat(fixed, golden, wide, decimal, ties))
	if err := writeBitsFile(filepath.Join(outDir, "float-json.txt"), jsonSet, jsonLine); err != nil {
		return err
	}
	if err := writeBitsFile(filepath.Join(outDir, "float-v.txt"), jsonSet, verbLine); err != nil {
		return err
	}

	formatSet := dedupe(slices.Concat(fixed, ties[:formatTies], wide[:formatWide], decimal[:formatDecimal]))
	return writeFormatFile(filepath.Join(outDir, "float-format.txt"), formatSet, golden)
}

func jsonLine(v float64) (string, error) {
	out, err := json.Marshal(v)
	if err != nil {
		if _, ok := errors.AsType[*json.UnsupportedValueError](err); ok {
			return "error", nil
		}
		return "", err
	}
	return string(out), nil
}

func verbLine(v float64) (string, error) {
	out := fmt.Sprintf("%v", v)
	if want := strconv.FormatFloat(v, 'g', -1, 64); out != want {
		return "", fmt.Errorf("%%v of %#x is %q but FormatFloat g -1 is %q", math.Float64bits(v), out, want)
	}
	return out, nil
}

func writeBitsFile(path string, values []float64, line func(float64) (string, error)) error {
	var b bytes.Buffer
	for _, v := range values {
		out, err := line(v)
		if err != nil {
			return err
		}
		fmt.Fprintf(&b, "%016x\t%s\n", math.Float64bits(v), out)
	}
	return os.WriteFile(path, b.Bytes(), 0o644)
}

func writeFormatFile(path string, values, golden []float64) error {
	var b bytes.Buffer
	for _, spec := range formatSpecs {
		set := values
		if goldenSpecs[spec] {
			set = dedupe(slices.Concat(values, golden))
		}
		for _, v := range set {
			if spec.fmt == 'f' && !math.IsNaN(v) && !math.IsInf(v, 0) && v != 0 {
				if exp := math.Ilogb(v); exp > fixedFormatMaxExp || exp < -fixedFormatMaxExp {
					continue
				}
			}
			fmt.Fprintf(&b, "%c\t%d\t%016x\t%s\n", spec.fmt, spec.prec, math.Float64bits(v), strconv.FormatFloat(v, spec.fmt, spec.prec, 64))
		}
	}
	return os.WriteFile(path, b.Bytes(), 0o644)
}

func dedupe(values []float64) []float64 {
	seen := make(map[uint64]bool, len(values))
	out := make([]float64, 0, len(values))
	for _, v := range values {
		bits := math.Float64bits(v)
		if !seen[bits] {
			seen[bits] = true
			out = append(out, v)
		}
	}
	return out
}

func fixedFloats() []float64 {
	next := func(v float64) float64 { return math.Nextafter(v, math.Inf(1)) }
	prev := func(v float64) float64 { return math.Nextafter(v, math.Inf(-1)) }
	base := []float64{
		0, 1, 0.1, 0.2, 0.3, 0.5, 1.5, 2.5, 0.125, 0.375, 1.0 / 3, 2.0 / 3, math.Pi, math.E, 0.1 + 0.2,
		1e-7, 1e-6, 1e-5, 1e-4, 1e-3, 0.01, 0.000001, 9.999999e-7, 1.5e-7, 2.5e-6, 123456e-12,
		10, 100, 123456, 999999, 1e6, 1234567, 1e7, 123456789.5, 1e15, 1e16, 1e17, 1e20, 1e21, 1e22, 1e23,
		123456789012345678, 8.41e21, 4.35, 0.3 + 0.6, 2.675, 1.005, 1e100, 1e-100, 1e300, 1e-300,
		float64(1 << 53), float64(1<<53) + 2, float64(1 << 63), math.Ldexp(1, 64),
		math.MaxFloat64, math.SmallestNonzeroFloat64, 2.2250738585072014e-308, 2.225073858507201e-308,
		math.Float64frombits(0x000fffffffffffff), math.Float64frombits(0x0010000000000000),
		3, 7, 17, 0.17, 0.9, 0.999, 0.9995, 0.05, 0.95, 0.0005, 99.5, 0.0449, 5e-324, 1.7976931348623157e308,
	}
	var out []float64
	for _, v := range base {
		out = append(out, v, -v, next(v), prev(v))
	}
	out = append(out,
		math.Copysign(0, -1), math.Inf(1), math.Inf(-1),
		math.Float64frombits(0x7ff8000000000001), math.Float64frombits(0xfff8000000000001), math.Float64frombits(0x7ff0000000000001),
	)
	return out
}

func wideFloats(rng *rand.Rand, n int) []float64 {
	out := make([]float64, 0, n)
	for len(out) < n {
		v := math.Float64frombits(rng.Uint64())
		if math.IsNaN(v) || math.IsInf(v, 0) {
			continue
		}
		out = append(out, v)
	}
	return out
}

func decimalFloats(rng *rand.Rand, n int) []float64 {
	out := make([]float64, 0, n)
	for len(out) < n {
		digits := 1 + rng.IntN(17)
		var mantissa strings.Builder
		for range digits {
			mantissa.WriteByte(byte('0' + rng.IntN(10)))
		}
		text := mantissa.String() + "e" + strconv.Itoa(rng.IntN(61)-45)
		v, err := strconv.ParseFloat(text, 64)
		if err != nil {
			continue
		}
		if rng.IntN(2) == 0 {
			v = -v
		}
		out = append(out, v)
	}
	return out
}

func tieFloats(rng *rand.Rand, n int) []float64 {
	out := make([]float64, 0, n)
	for len(out) < n {
		k := float64(rng.IntN(1 << 20))
		scale := math.Ldexp(1, -rng.IntN(24))
		v := k * scale
		if rng.IntN(2) == 0 {
			v = -v
		}
		out = append(out, v)
	}
	return out
}

func goldenFloats(root string) ([]float64, error) {
	seen := map[uint64]bool{}
	var out []float64
	collect := func(token json.Number) {
		v, err := strconv.ParseFloat(string(token), 64)
		if err != nil || math.IsInf(v, 0) {
			return
		}
		bits := math.Float64bits(v)
		if !seen[bits] {
			seen[bits] = true
			out = append(out, v)
		}
	}
	for _, dir := range []string{"cmd", "internal", "tools"} {
		err := filepath.WalkDir(filepath.Join(root, dir), func(path string, d fs.DirEntry, err error) error {
			if err != nil || d.IsDir() {
				return err
			}
			name := d.Name()
			gz := strings.HasSuffix(name, ".gz")
			name = strings.TrimSuffix(name, ".gz")
			if !strings.HasSuffix(name, ".json") && !strings.HasSuffix(name, ".jsonl") {
				return nil
			}
			data, err := readMaybeGzip(path, gz)
			if err != nil {
				return err
			}
			for _, chunk := range jsonDocuments(data, strings.HasSuffix(name, ".jsonl")) {
				walkNumbers(chunk, collect)
			}
			return nil
		})
		if err != nil {
			return nil, err
		}
	}
	slices.SortFunc(out, func(a, b float64) int {
		return compareBits(math.Float64bits(a), math.Float64bits(b))
	})
	return out, nil
}

func compareBits(a, b uint64) int {
	switch {
	case a < b:
		return -1
	case a > b:
		return 1
	}
	return 0
}

func readMaybeGzip(path string, gz bool) ([]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	var r io.Reader = f
	if gz {
		zr, err := gzip.NewReader(f)
		if err != nil {
			return nil, fmt.Errorf("%s: %w", path, err)
		}
		defer zr.Close()
		r = zr
	}
	return io.ReadAll(bufio.NewReader(r))
}

func jsonDocuments(data []byte, lines bool) [][]byte {
	if !lines {
		return [][]byte{data}
	}
	return bytes.Split(data, []byte("\n"))
}

func walkNumbers(doc []byte, visit func(json.Number)) {
	dec := json.NewDecoder(bytes.NewReader(doc))
	dec.UseNumber()
	for {
		var value any
		if err := dec.Decode(&value); err != nil {
			return
		}
		walkValue(value, visit)
	}
}

func walkValue(value any, visit func(json.Number)) {
	switch v := value.(type) {
	case json.Number:
		visit(v)
	case []any:
		for _, item := range v {
			walkValue(item, visit)
		}
	case map[string]any:
		for _, item := range v {
			walkValue(item, visit)
		}
	}
}
