package watch

import (
	"os"
	"testing"
	"time"
)

// Frames show local times; pin the zone so goldens do not depend on the machine's.
func TestMain(m *testing.M) {
	time.Local = time.UTC
	os.Exit(m.Run())
}
