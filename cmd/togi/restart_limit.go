package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"strings"

	"github.com/shgew/togi/internal/hardware"
	"github.com/shgew/togi/internal/tuningboot"
)

const restartLimitHelp = `Usage: togi restart-limit --tuning-boot <grubenv>

Recover after the tuning service reaches its restart limit. Persist the count
and leave reason in the GRUB environment before requesting a reboot. The first
two consecutive restart-limit hits reboot back into the tuning boot; the third
clears and verifies saved_entry so the reboot selects the normal system. The
first durable journal append by run resets this count.

A retry creates /run/togi-retry-tuning-boot after the GRUB write succeeds, so the
shutdown service keeps the tuning entry selected. A failed reboot removes the
marker. Errors exit unsuccessfully so the service can fall back to a normal boot.
This command does not open the journal or access the CPU hardware.

Examples:
  sudo togi restart-limit --tuning-boot /boot/grub/grubenv`

type restartLimitOperations struct {
	bootloader func(string) tuningboot.Bootloader
	create     func() error
	remove     func() error
	command    func(context.Context, string, ...string) ([]byte, error)
}

func runRestartLimit(g *globals, args []string, stdout, stderr io.Writer) int {
	return runRestartLimitWith(g, args, stdout, stderr, restartLimitOperations{
		bootloader: func(env string) tuningboot.Bootloader { return hardware.GRUB{Env: env} },
		create:     createRetryMarker,
		remove:     removeRetryMarker,
		command:    commandOutput,
	})
}

func runRestartLimitWith(g *globals, args []string, stdout, stderr io.Writer, operations restartLimitOperations) int {
	var grubenv string
	flags := newFlagSet("restart-limit", g)
	flags.StringVar(&grubenv, "tuning-boot", "", "GRUB environment `file` for retry state and the saved tuning entry (required)")
	if code, ok := parseFlags(flags, args, restartLimitHelp, stdout, stderr); !ok {
		return code
	}
	if grubenv == "" {
		fmt.Fprintln(stderr, "togi restart-limit: --tuning-boot is required")
		commandUsage(flags, restartLimitHelp, stderr)
		return exitUsage
	}
	count, retry, err := tuningboot.RestartLimit(operations.bootloader(grubenv))
	if err != nil {
		fmt.Fprintf(stderr, "togi restart-limit: %v\n", err)
		return exitError
	}
	if retry {
		if err := operations.create(); err != nil {
			fmt.Fprintf(stderr, "togi restart-limit: create retry marker: %v\n", err)
			return exitError
		}
		fmt.Fprintf(stderr, "togi restart-limit: attempt %d of 3; rebooting back into the tuning boot\n", count)
	} else {
		if err := operations.remove(); err != nil {
			fmt.Fprintf(stderr, "togi restart-limit: remove retry marker: %v\n", err)
			return exitError
		}
		fmt.Fprintf(stderr, "togi restart-limit: attempt %d of 3; rebooting into the normal system\n", count)
	}
	if out, err := rebootSystem(operations.command); err != nil {
		fmt.Fprintf(stderr, "togi restart-limit: systemctl reboot: %v: %s\n", err, strings.TrimSpace(string(out)))
		if retry {
			if err := operations.remove(); err != nil {
				fmt.Fprintf(stderr, "togi restart-limit: remove retry marker after failed reboot: %v\n", err)
			}
		}
		return exitError
	}
	return exitOK
}

func createRetryMarker() error {
	file, err := os.OpenFile(tuningboot.RetryMarker, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		return err
	}
	if err := file.Close(); err != nil {
		return errors.Join(err, removeRetryMarker())
	}
	return nil
}

func removeRetryMarker() error {
	err := os.Remove(tuningboot.RetryMarker)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	return err
}
