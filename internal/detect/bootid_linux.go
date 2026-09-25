package detect

import (
	"fmt"
	"os"
	"strings"
)

func BootID() (string, error) {
	b, err := os.ReadFile("/proc/sys/kernel/random/boot_id")
	if err != nil {
		return "", fmt.Errorf("read boot id: %w", err)
	}
	return strings.TrimSpace(string(b)), nil
}
