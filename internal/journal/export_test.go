package journal

import (
	"fmt"
	"path/filepath"
)

type FilesystemProbeForTest struct{ faults *faultJournalFilesystem }

func FailFilesystemForTest(j *Journal, at int, after bool) *FilesystemProbeForTest {
	faults := &faultJournalFilesystem{journalFilesystem: j.fs, at: at, after: after}
	j.fs = faults
	if j.f != nil {
		j.f = faultJournalFile{journalFile: j.f, fs: faults, path: filepath.Join(j.dir, eventsFile)}
	}
	return &FilesystemProbeForTest{faults: faults}
}

func (p *FilesystemProbeForTest) Fired() bool { return p.faults.fired }

func (p *FilesystemProbeForTest) Calls() []string {
	steps := make([]string, len(p.faults.calls))
	for i, call := range p.faults.calls {
		steps[i] = fmt.Sprintf("%s %s -> %s", call.op, filepath.Base(call.path), filepath.Base(call.target))
	}
	return steps
}
