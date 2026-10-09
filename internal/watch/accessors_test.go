package watch

func (e entry) sentence() string {
	before, alarm, after := e.sentenceParts()
	return before + alarm + after
}

func (s *source) snapshot() Snapshot {
	s.reload()
	return s.snap
}
