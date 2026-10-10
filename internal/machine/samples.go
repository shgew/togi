package machine

import (
	"bufio"
	"encoding/json"
	"iter"
	"path/filepath"

	"github.com/shgew/togi/internal/trialfiles"
)

// ReadSamples yields the complete samples of the trial directory dir, from either form of its samples file.
func ReadSamples(dir string) iter.Seq[TrialConditions] {
	return func(yield func(TrialConditions) bool) {
		f, err := trialfiles.Open(filepath.Join(dir, trialfiles.Samples))
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
