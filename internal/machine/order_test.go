package machine

import (
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestOrder(t *testing.T) {
	var info []CoreInfo
	for i := 15; i >= 0; i-- {
		info = append(info, CoreInfo{Core: i, CCD: i / 8})
	}
	want := []int{0, 8, 1, 9, 2, 10, 3, 11, 4, 12, 5, 13, 6, 14, 7, 15}
	if diff := cmp.Diff(want, Order(info)); diff != "" {
		t.Fatalf("order (-want +got):\n%s", diff)
	}
}
