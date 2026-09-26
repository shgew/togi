//go:build !linux

package hardware

import (
	"errors"
	"fmt"

	"github.com/shgew/shycler/internal/config"
	"github.com/shgew/shycler/internal/machine"
)

var errPlatform = fmt.Errorf("hardware runs need Linux: %w", errors.ErrUnsupported)

// CheckPlatform reports whether this platform can run on real hardware.
func CheckPlatform() error { return errPlatform }

func New(config.Config, string) (machine.Machine, error) { return machine.Machine{}, errPlatform }
