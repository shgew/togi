package machine

import (
	"encoding/json"
	"fmt"
	"iter"
	"slices"
	"strconv"
	"strings"
)

// PerCoreMax bounds core IDs: the supported topology is 0–15 (docs/spec/runtime.md, preflight check 7).
const PerCoreMax = 16

var stringOrder = func() []int {
	order := make([]int, PerCoreMax)
	for i := range order {
		order[i] = i
	}
	slices.SortFunc(order, func(a, b int) int { return strings.Compare(strconv.Itoa(a), strconv.Itoa(b)) })
	return order
}()

// PerCore holds one reading per core without allocating, so copies of a sample never share state. It encodes as a JSON
// object keyed by core ID with keys sorted as strings, exactly as a map[int]T does, and omits itself when empty.
type PerCore[T ~int | ~int64] struct {
	values  [PerCoreMax]T
	present uint16
}

// PerCoreFrom collects the readings of cores 0–15; it panics on a core outside that range.
func PerCoreFrom[T ~int | ~int64](readings map[int]T) PerCore[T] {
	var p PerCore[T]
	for core, v := range readings {
		if !p.Set(core, v) {
			panic(fmt.Sprintf("core %d outside 0–%d", core, PerCoreMax-1))
		}
	}
	return p
}

// Set records core's reading and reports false, recording nothing, when core is outside 0–15.
func (p *PerCore[T]) Set(core int, v T) bool {
	if core < 0 || core >= PerCoreMax {
		return false
	}
	p.values[core] = v
	p.present |= 1 << core
	return true
}

func (p PerCore[T]) Get(core int) (T, bool) {
	if core < 0 || core >= PerCoreMax || p.present&(1<<core) == 0 {
		return 0, false
	}
	return p.values[core], true
}

func (p PerCore[T]) Len() int {
	n := 0
	for present := p.present; present != 0; present &= present - 1 {
		n++
	}
	return n
}

// All yields the recorded cores in ascending order.
func (p PerCore[T]) All() iter.Seq2[int, T] {
	return func(yield func(int, T) bool) {
		for core := range PerCoreMax {
			if p.present&(1<<core) != 0 && !yield(core, p.values[core]) {
				return
			}
		}
	}
}

func (p PerCore[T]) IsZero() bool { return p.present == 0 }

func (p PerCore[T]) Equal(other PerCore[T]) bool { return p == other }

func (p PerCore[T]) MarshalJSON() ([]byte, error) {
	b := make([]byte, 0, 2+p.Len()*12)
	b = append(b, '{')
	first := true
	for _, core := range stringOrder {
		if p.present&(1<<core) == 0 {
			continue
		}
		if !first {
			b = append(b, ',')
		}
		first = false
		b = append(b, '"')
		b = strconv.AppendInt(b, int64(core), 10)
		b = append(b, `":`...)
		b = strconv.AppendInt(b, int64(p.values[core]), 10)
	}
	return append(b, '}'), nil
}

func (p *PerCore[T]) UnmarshalJSON(data []byte) error {
	var readings map[int]T
	if err := json.Unmarshal(data, &readings); err != nil {
		return err
	}
	*p = PerCore[T]{}
	for core, v := range readings {
		if !p.Set(core, v) {
			return fmt.Errorf("core %d outside 0–%d", core, PerCoreMax-1)
		}
	}
	return nil
}
