package machine

import (
	"bufio"
	"encoding/json"
	"iter"
	"os"
	"path/filepath"
)

func ReadSamples(dir string) iter.Seq[TrialConditions] {
	return func(yield func(TrialConditions) bool) {
		f, err := os.Open(filepath.Join(dir, "samples.jsonl"))
		if err != nil {
			return
		}
		defer f.Close()
		reader := bufio.NewReader(f)
		for {
			line, err := reader.ReadBytes('\n')
			if err != nil {
				return
			}
			var sample TrialConditions
			if json.Unmarshal(line, &sample) == nil && !yield(sample) {
				return
			}
		}
	}
}
