package journal

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"

	"github.com/shgew/togi/internal/machine"
)

func RecordedContext(dir string) (*machine.BIOSContext, error) {
	path := filepath.Join(dir, eventsFile)
	data, err := os.ReadFile(path)
	if errors.Is(err, fs.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("read journal %s: %w", path, err)
	}
	for n := 1; ; n++ {
		line, rest, ok := bytes.Cut(data, []byte{'\n'})
		if !ok {
			return nil, nil
		}
		data = rest
		var env envelope
		if err := json.Unmarshal(line, &env); err != nil {
			return nil, fmt.Errorf("read journal %s line %d: %w", path, n, err)
		}
		if env.Kind == "" {
			return nil, fmt.Errorf("read journal %s line %d: event has no kind", path, n)
		}
		if n == 1 && env.Kind != KindSessionStart {
			return nil, fmt.Errorf("read journal %s line 1: first event is %s, want %s", path, env.Kind, KindSessionStart)
		}
		if env.Kind != KindSessionContext {
			continue
		}
		var p SessionContext
		if err := json.Unmarshal(line, &p); err != nil {
			return nil, fmt.Errorf("read journal %s line %d: decode session.context: %w", path, n, err)
		}
		return &p.BIOSContext, nil
	}
}
