package main

import (
	"bufio"
	"fmt"
	"math"
	"math/rand/v2"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"time"
)

const (
	convSeed    = 702
	convRandoms = 4000
)

func init() {
	modes["convert"] = writeConvert
	modes["convert-amd64"] = writeConvertAMD64
}

// convSink keeps the compiler from folding a conversion whose input it can see, because Go
// defines constant conversions at compile time and the vectors need what the CPU does.
var convSink []float64

type convFile struct {
	path string
	w    *bufio.Writer
	f    *os.File
}

func convCreate(dir, name string) (*convFile, error) {
	f, err := os.Create(filepath.Join(dir, name))
	if err != nil {
		return nil, err
	}
	return &convFile{path: name, w: bufio.NewWriter(f), f: f}, nil
}

func (c *convFile) line(fields ...any) {
	parts := make([]string, len(fields))
	for i, f := range fields {
		switch v := f.(type) {
		case float64:
			parts[i] = fmt.Sprintf("%016x", math.Float64bits(v))
		case float32:
			parts[i] = fmt.Sprintf("%08x", math.Float32bits(v))
		case time.Duration:
			parts[i] = fmt.Sprint(int64(v))
		default:
			parts[i] = fmt.Sprint(v)
		}
	}
	fmt.Fprintln(c.w, strings.Join(parts, "\t"))
}

func (c *convFile) close() error {
	if err := c.w.Flush(); err != nil {
		return err
	}
	return c.f.Close()
}

func convRand(stream uint64) *rand.Rand {
	return rand.New(rand.NewPCG(convSeed, stream))
}

func convDedupe(values []float64) []float64 {
	seen := map[uint64]bool{}
	out := values[:0:0]
	for _, v := range values {
		if bits := math.Float64bits(v); !seen[bits] {
			seen[bits] = true
			out = append(out, v)
		}
	}
	return out
}

func convNaNs() []float64 {
	return []float64{
		math.NaN(),
		math.Float64frombits(0x7ff0000000000001),
		math.Float64frombits(0x7ff8000000000001),
		math.Float64frombits(0xfff8000000000000),
		math.Float64frombits(0xffffffffffffffff),
	}
}

func convAround(x float64, n int) []float64 {
	out := []float64{x}
	up, down := x, x
	for range n {
		up, down = math.Nextafter(up, math.Inf(1)), math.Nextafter(down, math.Inf(-1))
		out = append(out, up, down)
	}
	return out
}

// convFloatToInt64 are the float inputs of Go's int(x) at the sites that convert a rounded,
// scaled or ratio value, the exact boundaries of an int64, and what is left over.
func convFloatToInt64() []float64 {
	var in []float64
	in = append(in, 0, math.Copysign(0, -1), 0.5, -0.5, 1, -1, 1.5, -1.5, 2.5, -2.5, 0.9999999999999999, -0.9999999999999999, 1e-9, -1e-9,
		math.SmallestNonzeroFloat64, -math.SmallestNonzeroFloat64, math.MaxFloat64, -math.MaxFloat64, math.Inf(1), math.Inf(-1))
	in = append(in, convNaNs()...)
	for _, p := range []float64{31, 32, 52, 53, 62, 63, 64, 65, 100, 1023} {
		in = append(in, convAround(math.Ldexp(1, int(p)), 3)...)
		in = append(in, convAround(-math.Ldexp(1, int(p)), 3)...)
	}
	in = append(in, convAround(math.Ldexp(1, 63)-1024, 2)...)
	in = append(in, convAround(-math.Ldexp(1, 63)-2048, 2)...)
	in = append(in, convAround(float64(math.MaxInt32), 2)...)
	in = append(in, convAround(float64(math.MinInt32), 2)...)
	in = append(in, 9.223372036854775e18, 9.2233720368547758e18, -9.2233720368547758e18, 1e19, -1e19)
	for _, v := range []float64{0.9, 1.0125, 1.1875, 0.95, 1.35, 3.5, 4.4, 5500, 4750.4, 3200.5, 3200.49999999, 49.9999999999, 50.0000000001, 71.1111, 249.999999999} {
		in = append(in, v, -v, v*250+1e-9, -v*250-1e-9, v/0.00625-1e-9, (v-1)/0.00625+1e-9, v*3600, v/1000, v*1e9, v*float64(len("abcdefgh")))
	}
	r := convRand(1)
	for range convRandoms {
		in = append(in, r.Float64()*2e4-1e4, r.Float64()*20-10, math.Ldexp(r.Float64()*2-1, r.IntN(140)-5), math.Float64frombits(r.Uint64()))
	}
	for k := -300; k <= 300; k++ {
		in = append(in, float64(k)+0.5, float64(k)-0.5, float64(k)/3, float64(k)*0.1)
	}
	return convDedupe(in)
}

func convSecondsInputs() []float64 {
	var in []float64
	in = append(in, 0, math.Copysign(0, -1), 1, -1, 0.5, 1e-9, -1e-9, 4.9999999999e-10, 5e-10, 5.00000001e-10, 0.001, 0.0000005, 60, 600, 3600, 86400, 1e6,
		9.223372036854775e9, 9.223372036854776e9, 9.3e9, -9.223372036854775e9, -9.3e9, 1e10, 1e300, math.Inf(1), math.Inf(-1), math.MaxFloat64)
	in = append(in, convNaNs()...)
	in = append(in, convAround(math.Ldexp(1, 63)/1e9, 3)...)
	in = append(in, convAround(-math.Ldexp(1, 63)/1e9, 3)...)
	for _, v := range []float64{0.35, 1.5, 12.34, 29.97, 300.25, 599.9999, 601.0001} {
		in = append(in, v, -v, v*0.5, v+0.1)
	}
	r := convRand(2)
	for range convRandoms {
		in = append(in, r.Float64()*1200, r.Float64()*2e-6, math.Ldexp(r.Float64(), r.IntN(80)-20), (r.Float64()-0.5)*2e10)
	}
	return convDedupe(in)
}

func convSpeedProducts() [][2]float64 {
	var out [][2]float64
	elapsed := []float64{0, 1, 1e9, 1.5e9, 3e11, 9.223372036854775e18, 9.223372036854776e18, 1.7e18}
	speeds := []float64{0, 0.25, 0.5, 1, 1.5, 2, 3, 10, 100, 1e6, 1e12, math.Inf(1), math.NaN()}
	for _, e := range elapsed {
		for _, s := range speeds {
			out = append(out, [2]float64{e, s})
		}
	}
	r := convRand(3)
	for range convRandoms {
		out = append(out, [2]float64{math.Ldexp(r.Float64(), r.IntN(64)), r.Float64() * 10})
	}
	return out
}

func writeConvertAMD64(dir string) error {
	if runtime.GOARCH != "amd64" {
		return fmt.Errorf("the float-to-integer vectors are what amd64 does, not %s", runtime.GOARCH)
	}
	file, err := convCreate(dir, "convert-amd64.txt")
	if err != nil {
		return err
	}
	convSink = convFloatToInt64()
	for _, v := range convSink {
		file.line("i64", v, int64(v))
	}
	convSink = convSecondsInputs()
	for _, v := range convSink {
		file.line("dur-secs", v, time.Duration(v*float64(time.Second)))
	}
	pairs := convSpeedProducts()
	for _, p := range pairs {
		file.line("dur-scaled", p[0], p[1], time.Duration(p[0]*p[1]))
	}
	return file.close()
}

func writeConvert(dir string) error {
	file, err := convCreate(dir, "convert.txt")
	if err != nil {
		return err
	}
	convFloat32(file)
	convRounding(file)
	convDurations(file)
	convIntegers(file)
	return file.close()
}

func convFloat32Inputs() []float64 {
	var in []float64
	in = append(in, 0, math.Copysign(0, -1), 1, -1, 0.1, 0.9, 1.0125, 1.1875, 1.35, 65.4321, 4.4, 0.00625, 1e-7, 16777216, 16777217, 16777219, -16777217,
		math.MaxFloat32, -math.MaxFloat32, math.SmallestNonzeroFloat32, -math.SmallestNonzeroFloat32, math.Inf(1), math.Inf(-1), math.MaxFloat64, math.SmallestNonzeroFloat64,
		1.1754943508222875e-38, 1.401298464324817e-45, 7.006492321624085e-46, 7.006492321624086e-46, 2.1019476964872256e-45)
	in = append(in, convNaNs()...)
	in = append(in, convAround(math.MaxFloat32, 4)...)
	in = append(in, convAround(float64(math.Nextafter32(math.MaxFloat32, 0)), 2)...)
	in = append(in, convAround(float64(math.MaxFloat32)+math.Ldexp(1, 103), 3)...)
	in = append(in, convAround(1+math.Ldexp(1, -24), 3)...)
	in = append(in, convAround(1+3*math.Ldexp(1, -24), 3)...)
	in = append(in, convAround(math.Ldexp(1, -149)/2, 3)...)
	in = append(in, convAround(math.Ldexp(1, -126), 3)...)
	r := convRand(4)
	for range convRandoms {
		in = append(in, 0.5+r.Float64(), r.Float64()*100, math.Ldexp(r.Float64()*2-1, r.IntN(300)-150), math.Float64frombits(r.Uint64()))
	}
	return convDedupe(in)
}

func convFloat32(file *convFile) {
	inputs := convFloat32Inputs()
	convSink = inputs
	for _, v := range inputs {
		f := float32(v)
		file.line("f32", v, f)
	}
	r := convRand(5)
	for range convRandoms {
		a := float32(0.5 + r.Float64())
		b := float32(r.Float64()*2 - 1)
		if r.IntN(4) == 0 {
			a = math.Float32frombits(r.Uint32())
			b = math.Float32frombits(r.Uint32())
		}
		file.line("f32-ops", a, b, a+b, a-b, a*b, a/b, float64(a), float64(a) > float64(b), a > b)
	}
	specials := []float32{0, float32(math.Copysign(0, -1)), 1, -1, math.MaxFloat32, math.SmallestNonzeroFloat32, float32(math.Inf(1)), float32(math.Inf(-1)), float32(math.NaN()), 16777216, 0.1}
	for _, a := range specials {
		for _, b := range specials {
			file.line("f32-ops", a, b, a+b, a-b, a*b, a/b, float64(a), float64(a) > float64(b), a > b)
		}
	}
}

func convRounding(file *convFile) {
	in := convFloatToInt64()
	for _, v := range in {
		file.line("math", v, math.Round(v), math.Floor(v), math.Ceil(v), math.Trunc(v))
	}
}

func convDurationInputs(randoms int) []time.Duration {
	in := []time.Duration{0, 1, -1, 7, 999, 1000, 1001, 999999, 1000000, 1000001, 1500, 1500000, 999999999, time.Second, time.Second + 1, time.Second - 1,
		59 * time.Second, time.Minute, time.Minute + time.Second, 90 * time.Second, 599*time.Second + 999*time.Millisecond, 10 * time.Minute, time.Hour, time.Hour + time.Minute + time.Second,
		25*time.Hour + 30*time.Millisecond, 100 * time.Hour, 1234567 * time.Millisecond, 2500 * time.Microsecond, 120500 * time.Microsecond,
		math.MaxInt64, math.MaxInt64 - 1, math.MinInt64, math.MinInt64 + 1, -time.Second, -time.Minute, -1500, -1500000, -90 * time.Second, -time.Hour - 1}
	for exp := range 19 {
		p := time.Duration(math.Pow10(exp))
		in = append(in, p, p-1, p+1, -p, -p+1, -p-1, 5*p, 5*p-1, 5*p+1, 15*p, 15*p/10*10)
	}
	r := convRand(6)
	for range randoms {
		in = append(in, time.Duration(r.Int64()>>r.UintN(63)), -time.Duration(r.Int64()>>r.UintN(63)), time.Duration(r.Int64N(int64(2*time.Hour))))
	}
	slices.Sort(in)
	return slices.Compact(in)
}

func convDurations(file *convFile) {
	durations := convDurationInputs(1000)
	for _, d := range durations {
		file.line("dur-string", d, d.String())
		file.line("dur-float", d, d.Seconds(), d.Minutes(), d.Hours())
		file.line("dur-int", d, d.Milliseconds(), d.Microseconds())
	}
	moduli := []time.Duration{time.Millisecond, time.Second, time.Microsecond, time.Minute, time.Hour, 7, 1, 1500 * time.Millisecond, 0, -1, -time.Second, math.MaxInt64, math.MinInt64}
	for _, d := range convDurationInputs(0) {
		for _, m := range moduli {
			file.line("dur-round", d, m, d.Round(m), d.Truncate(m))
		}
	}
	r := convRand(7)
	for range 2000 {
		d := time.Duration(r.Int64() >> r.UintN(63))
		m := time.Duration(r.Int64()>>r.UintN(63)) + 1
		file.line("dur-round", d, m, d.Round(m), d.Truncate(m))
	}
	counts := []int64{0, 1, -1, 2, 60, 600, 3600, 86400, 1e9, 9223372036, 9223372037, math.MaxInt64, math.MinInt64, -9223372036, -9223372037, 1 << 40, 1 << 33, 1 << 34}
	units := []time.Duration{time.Nanosecond, time.Microsecond, time.Millisecond, time.Second, time.Minute, time.Hour}
	for _, n := range counts {
		for _, u := range units {
			file.line("dur-mul", n, u, time.Duration(n)*u)
		}
	}
}

func convIntegers(file *convFile) {
	values := []int64{0, 1, -1, 2, -2, 3, -3, 7, -7, 8, -8, 9, 59, 60, 61, -59, -60, -61, 1000, -1000, 1 << 32, -(1 << 32), math.MaxInt64, math.MinInt64, math.MaxInt64 - 1, math.MinInt64 + 1}
	divisors := []int64{1, -1, 2, -2, 3, -3, 8, -8, 60, -60, 1000, -1000, math.MaxInt64, math.MinInt64, math.MinInt64 + 1}
	r := convRand(8)
	for range 100 {
		values = append(values, r.Int64()>>r.UintN(63), -(r.Int64() >> r.UintN(63)))
	}
	for _, a := range values {
		for _, b := range divisors {
			file.line("divmod", a, b, a/b, a%b)
		}
	}
	for _, x := range []int64{1, -1, 2, 3, 5, math.MaxInt64, math.MinInt64, 0} {
		for n := range uint(70) {
			file.line("shl", x, n, x<<n)
		}
		for _, n := range []uint{100, 255, 1 << 20, math.MaxUint32, math.MaxUint64} {
			file.line("shl", x, n, x<<n)
		}
	}
	for _, x := range []int{1, 3} {
		for n := range 70 {
			file.line("shl-int", x, n, x<<n)
		}
	}
}
