package hardware

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/shgew/togi/internal/machine"
)

func watchdog(root string) machine.Check {
	c := machine.Check{Name: "watchdog"}
	paths, err := filepath.Glob(filepath.Join(root, "sys/class/watchdog/watchdog*/state"))
	if err != nil {
		c.Detail = fmt.Sprintf("find hardware watchdogs: %v", err)
		return c
	}
	if len(paths) == 0 {
		c.Detail = "no hardware watchdog state available"
		return c
	}
	var details []string
	for _, path := range paths {
		name := filepath.Base(filepath.Dir(path))
		raw, err := os.ReadFile(path)
		if err != nil {
			details = append(details, fmt.Sprintf("read %s state: %v", name, err))
			continue
		}
		state := strings.TrimSpace(string(raw))
		if state != "active" {
			details = append(details, fmt.Sprintf("%s state %q, want active", name, state))
			continue
		}
		raw, err = os.ReadFile(filepath.Join(filepath.Dir(path), "identity"))
		if err != nil {
			details = append(details, fmt.Sprintf("read %s identity: %v", name, err))
			continue
		}
		identity := strings.TrimSpace(string(raw))
		if identity == "Software Watchdog" || identity == "" {
			details = append(details, fmt.Sprintf("%s is not a hardware watchdog (%q)", name, identity))
			continue
		}
		c.OK, c.Detail = true, fmt.Sprintf("%s (%s) active", name, identity)
		return c
	}
	c.Detail = strings.Join(details, "; ")
	return c
}
