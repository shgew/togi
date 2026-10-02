package main

import (
	"os"
	"runtime/debug"
	"testing"
)

// Rendering watch frames at every screen size allocates heavily; a larger heap target trades
// about 75 MB of peak memory for a seventh less CPU.
func TestMain(m *testing.M) {
	debug.SetGCPercent(400)
	os.Exit(m.Run())
}
