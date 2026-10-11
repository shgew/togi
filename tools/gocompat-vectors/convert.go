package main

import (
	"bufio"
	"fmt"
	"math"
	"math/rand/v2"
	"os"
	"os/exec"
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

// convert.txt holds the rows whose bits are the same on every architecture Go supports.
// A row whose result can differ between architectures goes to convert-amd64.txt and
// convert-arm64.txt instead: every float-to-integer conversion, and every float32, float32
// arithmetic and rounding row with a NaN in it, because the CPUs pick different NaN bits.
// convert-amd64 runs on amd64 only; convert-arm64 cross-builds this program and runs it
// under qemu-aarch64 unless it already runs on arm64.
func init() {
	modes["convert"] = writeConvert
	modes["convert-amd64"] = writeConvertAMD64
	modes["convert-arm64"] = writeConvertARM64
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

// convOut sends each row to the file of its kind: rows with a NaN in them go to arch, the
// rest to common. A nil file drops its rows.
type convOut struct {
	common, arch *convFile
}

func (o convOut) line(fields ...any) {
	file := o.common
	if convHasNaN(fields) {
		file = o.arch
	}
	if file != nil {
		file.line(fields...)
	}
}

func convHasNaN(fields []any) bool {
	for _, f := range fields {
		switch v := f.(type) {
		case float64:
			if math.IsNaN(v) {
				return true
			}
		case float32:
			if math.IsNaN(float64(v)) {
				return true
			}
		}
	}
	return false
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
		in = append(in, v, -v, float64(v*250)+1e-9, float64(-v*250)-1e-9, v/0.00625-1e-9, (v-1)/0.00625+1e-9, v*3600, v/1000, v*1e9, v*float64(len("abcdefgh")))
	}
	r := convRand(1)
	for range convRandoms {
		in = append(in, float64(r.Float64()*2e4)-1e4, float64(r.Float64()*20)-10, math.Ldexp(float64(r.Float64()*2)-1, r.IntN(140)-5), math.Float64frombits(r.Uint64()))
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
		return fmt.Errorf("the amd64 vectors are what amd64 does, not %s", runtime.GOARCH)
	}
	if err := requireGOAMD64v1(); err != nil {
		return err
	}
	return writeConvertArch(dir, "convert-amd64.txt")
}

func writeConvertARM64(dir string) error {
	if runtime.GOARCH == "arm64" {
		return writeConvertArch(dir, "convert-arm64.txt")
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
	abs, err := filepath.Abs(dir)
	if err != nil {
		return err
	}
	run := exec.Command("qemu-aarch64", bin, "convert-arm64", abs)
	run.Stderr = os.Stderr
	if err := run.Run(); err != nil {
		return fmt.Errorf("running the arm64 build under qemu-aarch64: %w", err)
	}
	return nil
}

// writeConvertArch writes the rows whose bits depend on the architecture Go runs on.
func writeConvertArch(dir, name string) error {
	file, err := convCreate(dir, name)
	if err != nil {
		return err
	}
	out := convOut{arch: file}
	convFloat32(out)
	convRounding(out)
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
	out := convOut{common: file}
	convFloat32(out)
	convRounding(out)
	convDurations(file)
	convParseDurations(file)
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
	in = append(in, convAround(1+float64(3*math.Ldexp(1, -24)), 3)...)
	in = append(in, convAround(math.Ldexp(1, -149)/2, 3)...)
	in = append(in, convAround(math.Ldexp(1, -126), 3)...)
	r := convRand(4)
	for range convRandoms {
		in = append(in, 0.5+r.Float64(), r.Float64()*100, math.Ldexp(float64(r.Float64()*2)-1, r.IntN(300)-150), math.Float64frombits(r.Uint64()))
	}
	return convDedupe(in)
}

func convFloat32(file convOut) {
	inputs := convFloat32Inputs()
	convSink = inputs
	for _, v := range inputs {
		f := float32(v)
		file.line("f32", v, f)
	}
	r := convRand(5)
	for range convRandoms {
		a := float32(0.5 + r.Float64())
		b := float32(float64(r.Float64()*2) - 1)
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

func convRounding(file convOut) {
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

// convParseDurationInputs are Go's own ParseDuration test tables, the int64 boundaries,
// fractions long enough to stop accumulating, and strings built from the grammar's pieces.
func convParseDurationInputs() []string {
	in := []string{
		// time_test.go parseDurationTests
		"0", "5s", "30s", "1478s", "-5s", "+5s", "-0", "+0", "5.0s", "5.6s", "5.s", ".5s", "1.0s", "1.00s", "1.004s", "1.0040s", "100.00100s",
		"10ns", "11us", "12\u00b5s", "12\u03bcs", "13ms", "14s", "15m", "16h", "3h30m", "10.5s4m", "-2m3.4s", "1h2m3s4ms5us6ns", "39h9m14.425s",
		"52763797000ns", "0.3333333333333333333h", "9007199254740993ns", "9223372036854775807ns", "9223372036854775.807us",
		"9223372036s854ms775us807ns", "-9223372036854775808ns", "-9223372036854775.808us", "-9223372036s854ms775us808ns",
		"-2562047h47m16.854775808s", "0.100000000000000000000h", "0.830103483285477580700h",
		// time_test.go parseDurationErrorTests
		"", "3", "-", "s", ".", "-.", ".s", "+.s", "1d", "\x85\x85", "\xffff", "hello \xffff world", "\uFFFD", "\uFFFD hello \uFFFD world",
		"9223372036854775810ns", "9223372036854775808ns", "-9223372036854775809ns", "9223372036854776us", "3000000h",
		"9223372036854775.808us", "9223372036854ms775us808ns",
		// boundaries and the float64 fraction path
		"2562047h47m16.854775807s", "2562047h47m16.854775808s", "-2562047h47m16.854775807s", "2562047h47m16.854775806s", "2562047h47m17s",
		"9223372036854775807.5ns", "9223372036854775807ns1ns", "9223372036854775806ns1ns", "-9223372036854775807ns-1ns", "9223372036854775808us",
		"0.5ns0.5ns", "0.5ns", "1.5ns", "2.5ns", "0.999999999ns", "0.9999999999999999999999ns", "0.5us0.5us0.5us", "0.0000000001s", "0.0000000005s",
		"0.1h0.1h", "1.123456789123456789s", "1.0000000000000000000000000000000001s", "00000000000000000000000000000000000001s",
		"0000000000000000000000.0000000000000000000005s", ".000000000000000000000000001h", "1.9999999999ns",
		"9223372036854775808.0ns", "9223372036854775807.9999999999999999999ns", "-9223372036854775808.5ns", "0.000000000000000000000000h",
		"1h1h", "1h-1m", "-1h-1m", "+-1s", "--1s", "++1s", "1s ", " 1s", "1 s", "1S", "1H", "1µs", "1\u00b5S", "1\u03bcs", "1\u00b5m", "1µ", "1m1", "1.s1", ".s1", "1..5s", "1.5.5s", "1e3s", "0x10s", "1_0s",
		"0s", "-0s", "+0s", "0ns", "00", "0.0", "0.", ".0", "-0.0s", "0h0m0s", "0.0h", "1000000000ns", "1000000us", "1000ms", "60s", "60m", "24h",
		"1nsns", "ns1", "1n", "1u", "1us1", "1msus", "1.5msus", "1.5ms2", "3.", "3.h", "3.m", "3.ns",
	}
	pieces := []string{"0", "1", "5", "9", "00", "12", "345", ".", ".5", "0.", "1.25", "-", "+", "h", "m", "s", "ns", "us", "\u00b5s", "\u03bcs", "ms", "d", "x", "\u00b5",
		"9223372036854775807", "9223372036854775808", "922337203685477580", "99999999999999999999", "3600000000000", "2562047", "0.3333333333333333333", "0.9999999999999999999999"}
	r := convRand(9)
	digits := func(n int) string {
		var d strings.Builder
		for range n {
			d.WriteByte(byte('0' + r.IntN(10)))
		}
		return d.String()
	}
	units := []string{"ns", "us", "\u00b5s", "\u03bcs", "ms", "s", "m", "h"}
	for range 6000 {
		var b strings.Builder
		if r.IntN(3) == 0 {
			b.WriteString([]string{"-", "+"}[r.IntN(2)])
		}
		for range 1 + r.IntN(5) {
			if r.IntN(5) == 0 {
				b.WriteString(pieces[r.IntN(len(pieces))])
			} else {
				b.WriteString(digits(r.IntN(20)))
				if r.IntN(2) == 0 {
					b.WriteString("." + digits(r.IntN(25)))
				}
			}
			if r.IntN(10) != 0 {
				b.WriteString(units[r.IntN(len(units))])
			}
		}
		in = append(in, b.String())
	}
	return slices.Compact(slices.Sorted(slices.Values(in)))
}

// convParseDurations writes time.ParseDuration: the input as hex so that no byte of it can
// break the line, then the nanoseconds or the word error.
func convParseDurations(file *convFile) {
	for _, s := range convParseDurationInputs() {
		d, err := time.ParseDuration(s)
		result := fmt.Sprint(int64(d))
		if err != nil {
			result = "error"
		}
		file.line("dur-parse", fmt.Sprintf("%x", s), result)
	}
}
