//go:build !linux

package main

import (
	"errors"
	"fmt"
	"os"
)

func quietInput(*os.File) (func() error, error) {
	return nil, fmt.Errorf("turn off terminal echo: %w", errors.ErrUnsupported)
}

func discardInput(*os.File) error {
	return fmt.Errorf("discard terminal input: %w", errors.ErrUnsupported)
}
