package main

import (
	"fmt"
	"os"

	"golang.org/x/sys/unix"
)

// quietInput turns off the terminal's echo and line buffering, leaving its signal keys, so Ctrl-C still sends SIGINT.
// The returned function restores the previous settings.
func quietInput(in *os.File) (func() error, error) {
	fd := int(in.Fd())
	saved, err := unix.IoctlGetTermios(fd, unix.TCGETS)
	if err != nil {
		return nil, fmt.Errorf("read terminal settings: %w", err)
	}
	quiet := *saved
	quiet.Lflag &^= unix.ECHO | unix.ICANON
	quiet.Cc[unix.VMIN], quiet.Cc[unix.VTIME] = 1, 0
	if err := unix.IoctlSetTermios(fd, unix.TCSETS, &quiet); err != nil {
		return nil, fmt.Errorf("turn off terminal echo: %w", err)
	}
	return func() error {
		if err := unix.IoctlSetTermios(fd, unix.TCSETS, saved); err != nil {
			return fmt.Errorf("restore terminal settings: %w", err)
		}
		return nil
	}, nil
}

// discardInput drops input the terminal received but nobody has read yet.
func discardInput(in *os.File) error {
	if err := unix.IoctlSetInt(int(in.Fd()), unix.TCFLSH, unix.TCIFLUSH); err != nil {
		return fmt.Errorf("discard terminal input: %w", err)
	}
	return nil
}
