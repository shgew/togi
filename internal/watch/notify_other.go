//go:build !linux

package watch

import (
	"context"
	"errors"
	"fmt"
)

func watchJournal(context.Context, string) (<-chan error, func(), error) {
	return nil, nil, fmt.Errorf("journal notifications need Linux: %w", errors.ErrUnsupported)
}
