package session

import (
	"os"
	"runtime/debug"
	"testing"
)

// Simulated boots re-decode and re-encode whole journals; a larger heap
// target halves GC work for about 100 MB of peak memory.
func TestMain(m *testing.M) {
	debug.SetGCPercent(400)
	os.Exit(m.Run())
}
