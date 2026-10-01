package session

import (
	"github.com/shgew/togi/internal/config"
	"github.com/shgew/togi/internal/journal"
)

func configSnapshot(c config.Config) journal.ConfigSnapshot {
	return journal.ConfigSnapshot{
		StartOffsets:   c.StartOffsets,
		CandidateEdges: c.CandidateEdges,
		Durations:      journal.ConfigDurations(c.Durations),
		Evidence:       journal.ConfigEvidence(c.Evidence),
		Guard:          journal.ConfigGuard(c.Guard),
		DeadEnds:       journal.ConfigDeadEnds(c.DeadEnds),
		Backends:       journal.ConfigBackends(c.Backends),
		BackendUser:    c.BackendUser,
	}
}
