// Package hardware assembles the real machine: host, preflight and GRUB.
package hardware

import (
	"bytes"
	"fmt"
	"os/exec"
	"slices"
	"strings"
)

// GRUB reads and writes the GRUB environment file Env with grub-editenv.
type GRUB struct {
	Env string
	run func(env string, args ...string) (string, error)
}

func (g GRUB) ClearSavedEntry() (before, after string, err error) {
	if before, err = g.Get("saved_entry"); err != nil {
		return "", "", err
	}
	if err := g.Unset("saved_entry"); err != nil {
		return before, before, err
	}
	if after, err = g.Get("saved_entry"); err != nil {
		return before, "", err
	}
	if after != "" {
		return before, after, fmt.Errorf("saved_entry still %s after unset", after)
	}
	return before, "", nil
}

func (g GRUB) Get(name string) (string, error) {
	if err := grubVariable(name); err != nil {
		return "", err
	}
	out, err := g.editenv("list")
	if err != nil {
		return "", err
	}
	for line := range strings.Lines(out) {
		if v, ok := strings.CutPrefix(strings.TrimSpace(line), name+"="); ok {
			return v, nil
		}
	}
	return "", nil
}

func (g GRUB) Set(values map[string]string) error {
	names := make([]string, 0, len(values))
	for name, value := range values {
		if err := grubVariable(name); err != nil {
			return err
		}
		if len(value) > 300 {
			return fmt.Errorf("GRUB variable %s exceeds 300 bytes", name)
		}
		for i := range len(value) {
			if value[i] < ' ' || value[i] > '~' {
				return fmt.Errorf("GRUB variable %s must be printable ASCII on one line", name)
			}
		}
		names = append(names, name)
	}
	if len(names) == 0 {
		return nil
	}
	slices.Sort(names)
	args := make([]string, 1, len(names)+1)
	args[0] = "set"
	for _, name := range names {
		args = append(args, name+"="+values[name])
	}
	_, err := g.editenv(args...)
	return err
}

func (g GRUB) Unset(names ...string) error {
	for _, name := range names {
		if err := grubVariable(name); err != nil {
			return err
		}
	}
	if len(names) == 0 {
		return nil
	}
	_, err := g.editenv(append([]string{"unset"}, names...)...)
	return err
}

func grubVariable(name string) error {
	if len(name) == 0 || len(name) > 64 {
		return fmt.Errorf("GRUB variable name must have 1 to 64 bytes")
	}
	for i := range len(name) {
		c := name[i]
		if c != '_' && (c < 'a' || c > 'z') && (c < 'A' || c > 'Z') && (c < '0' || c > '9') {
			return fmt.Errorf("invalid GRUB variable name %q", name)
		}
	}
	return nil
}

func (g GRUB) editenv(args ...string) (string, error) {
	if g.run != nil {
		return g.run(g.Env, args...)
	}
	return runGRUB(g.Env, args...)
}

func runGRUB(env string, args ...string) (string, error) {
	cmd := exec.Command("grub-editenv", append([]string{env}, args...)...)
	var stdout, stderr bytes.Buffer
	cmd.Stdout, cmd.Stderr = &stdout, &stderr
	if err := cmd.Run(); err != nil {
		msg := strings.TrimSpace(stderr.String())
		if msg == "" {
			return "", fmt.Errorf("grub-editenv %s %s: %w", env, args[0], err)
		}
		return "", fmt.Errorf("grub-editenv %s %s: %w: %s", env, args[0], err, msg)
	}
	return stdout.String(), nil
}
