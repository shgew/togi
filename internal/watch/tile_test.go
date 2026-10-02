package watch

import (
	"fmt"
	"strings"
	"testing"

	"github.com/charmbracelet/x/ansi"
	"github.com/shgew/togi/internal/journal"
)

func TestTileDecorationFrames(t *testing.T) {
	t.Parallel()
	tile := tile{id: 2, phase: journal.PhaseResident, number: -20, hasNumber: true, fail: new(-40), joint: []int{-21, -30}, trying: new(-22), loaded: true, hunt: true, masked: true}
	for _, width := range []int{20, 29, 55} {
		for _, m := range []mode{roomy, compact} {
			t.Run(fmt.Sprintf("%d-%d", width, m), func(t *testing.T) {
				golden(t, fmt.Sprintf("tile-%d-%d", width, m), ansi.Strip(strings.Join(tile.render(width, m == compact), "\n"))+"\n")
			})
		}
	}
}
