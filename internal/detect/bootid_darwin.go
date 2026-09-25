package detect

import (
	"fmt"
	"syscall"
)

func BootID() (string, error) {
	id, err := syscall.Sysctl("kern.bootsessionuuid")
	if err != nil {
		return "", fmt.Errorf("read boot id: sysctl kern.bootsessionuuid: %w", err)
	}
	return id, nil
}
