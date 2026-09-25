// Package hardware assembles the real machine: host, preflight and GRUB.
package hardware

import (
	"bytes"
	"fmt"
	"os/exec"
	"strings"
)

// GRUB clears the saved entry in the GRUB environment file Env with grub-editenv.
type GRUB struct{ Env string }

func (g GRUB) ClearSavedEntry() (before, after string, err error) {
	if before, err = g.savedEntry(); err != nil {
		return "", "", err
	}
	if _, err := g.editenv("unset", "saved_entry"); err != nil {
		return before, before, err
	}
	if after, err = g.savedEntry(); err != nil {
		return before, "", err
	}
	if after != "" {
		return before, after, fmt.Errorf("saved_entry still %s after unset", after)
	}
	return before, "", nil
}

func (g GRUB) savedEntry() (string, error) {
	out, err := g.editenv("list")
	if err != nil {
		return "", err
	}
	for line := range strings.Lines(out) {
		if v, ok := strings.CutPrefix(strings.TrimSpace(line), "saved_entry="); ok {
			return v, nil
		}
	}
	return "", nil
}

func (g GRUB) editenv(args ...string) (string, error) {
	cmd := exec.Command("grub-editenv", append([]string{g.Env}, args...)...)
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		msg := strings.TrimSpace(stderr.String())
		if msg == "" {
			return "", fmt.Errorf("grub-editenv %s %s: %w", g.Env, args[0], err)
		}
		return "", fmt.Errorf("grub-editenv %s %s: %w: %s", g.Env, args[0], err, msg)
	}
	return stdout.String(), nil
}
