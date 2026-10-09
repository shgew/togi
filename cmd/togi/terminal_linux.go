package main

import (
	"fmt"
	"os"

	"golang.org/x/sys/unix"
)

// quietInput turns off the terminal's echo and line buffering, leaving its signal keys, so Ctrl-C still sends SIGINT.
// The returned function restores the previous settings. A terminal that refuses the change keeps echoing; the
// dashboard is only less tidy then, so there is nothing to report.
func quietInput(in *os.File) (restore func()) {
	fd := int(in.Fd())
	saved, err := unix.IoctlGetTermios(fd, unix.TCGETS)
	if err != nil {
		return func() {}
	}
	quiet := *saved
	quiet.Lflag &^= unix.ECHO | unix.ICANON
	quiet.Cc[unix.VMIN], quiet.Cc[unix.VTIME] = 1, 0
	if err := unix.IoctlSetTermios(fd, unix.TCSETS, &quiet); err != nil {
		return func() {}
	}
	return func() { _ = unix.IoctlSetTermios(fd, unix.TCSETS, saved) }
}

// discardInput drops input the terminal received but nobody has read yet.
func discardInput(in *os.File) error {
	if err := unix.IoctlSetInt(int(in.Fd()), unix.TCFLSH, unix.TCIFLUSH); err != nil {
		return fmt.Errorf("discard terminal input: %w", err)
	}
	return nil
}
