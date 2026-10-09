//go:build !linux

package main

import (
	"errors"
	"fmt"
	"os"
)

// quietInput leaves the terminal as it is: echo control is implemented for Linux only.
func quietInput(*os.File) (restore func()) { return func() {} }

func discardInput(*os.File) error {
	return fmt.Errorf("discard terminal input: %w", errors.ErrUnsupported)
}
