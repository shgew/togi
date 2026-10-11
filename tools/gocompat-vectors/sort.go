package main

import (
	"bufio"
	"fmt"
	"math"
	"math/rand/v2"
	"os"
	"path/filepath"
	"slices"
	"strings"
)

const sortSeed = 704

// sort.txt holds slices.Sort on float64 slices, input then output as float64 bits. Equal
// values that differ in bits (+0 and -0, NaNs with different payloads) show which
// permutation pdqsort chose, which any other correct sort can change.
func init() {
	modes["sort"] = writeSort
}

var sortPool = []float64{
	0,
	math.Copysign(0, -1),
	1,
	-1,
	0.5,
	math.Inf(1),
	math.Inf(-1),
	math.Float64frombits(0x7ff8000000000001),
	math.Float64frombits(0x7ff8000000000002),
	math.Float64frombits(0xfff8000000000000),
}

func writeSort(outDir string) error {
	f, err := os.Create(filepath.Join(outDir, "sort.txt"))
	if err != nil {
		return err
	}
	w := bufio.NewWriter(f)
	rng := rand.New(rand.NewPCG(sortSeed, 0))
	pick := func(pool []float64) float64 { return pool[rng.IntN(len(pool))] }
	zeros := sortPool[:2]
	var cases [][]float64
	sizes := []int{100, 257, 1000}
	for n := range 71 {
		sizes = append(sizes, n)
	}
	slices.Sort(sizes)
	for _, n := range sizes {
		values := make([]float64, n)
		for i := range values {
			values[i] = pick(sortPool)
		}
		cases = append(cases, values)
	}
	for _, n := range []int{13, 60, 200, 1000} {
		ascending := make([]float64, n)
		descending := make([]float64, n)
		equal := make([]float64, n)
		sawtooth := make([]float64, n)
		organ := make([]float64, n)
		nearlySorted := make([]float64, n)
		for i := range n {
			ascending[i] = float64(i/3) + pick(zeros)
			descending[i] = float64((n-i)/3) + pick(zeros)
			equal[i] = pick(zeros)
			sawtooth[i] = float64(i%7) + pick(zeros)
			organ[i] = float64(min(i, n-i)) + pick(zeros)
			nearlySorted[i] = float64(i) + pick(zeros)
		}
		for range 3 {
			i, j := rng.IntN(n), rng.IntN(n)
			nearlySorted[i], nearlySorted[j] = nearlySorted[j], nearlySorted[i]
		}
		cases = append(cases, ascending, descending, equal, sawtooth, organ, nearlySorted)
	}
	for _, values := range cases {
		in := sortBits(values)
		slices.Sort(values)
		fmt.Fprintf(w, "sort\t%s\t%s\n", in, sortBits(values))
	}
	if err := w.Flush(); err != nil {
		f.Close()
		return err
	}
	return f.Close()
}

func sortBits(values []float64) string {
	parts := make([]string, len(values))
	for i, v := range values {
		parts[i] = fmt.Sprintf("%016x", math.Float64bits(v))
	}
	return strings.Join(parts, ",")
}
