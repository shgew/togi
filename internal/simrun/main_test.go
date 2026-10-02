package simrun

import (
	"os"
	"runtime/debug"
	"testing"
)

// Whole simulated sessions allocate heavily; a larger heap target trades
// about 400 MB of peak memory for a quarter less GC work.
func TestMain(m *testing.M) {
	debug.SetGCPercent(400)
	os.Exit(m.Run())
}
