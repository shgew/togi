package main

import "github.com/shgew/togi/internal/journal"

const (
	exitOK            = 0
	exitError         = 1
	exitUsage         = 2
	exitLocked        = 3
	exitFailureAtZero = 10
	exitSMU           = 11
	exitNoEvidence    = 12
	exitBootLoop      = 13
	exitContainment   = 14
	exitPreflight     = 15
	exitIncompatible  = 16
	exitDefect        = 17
)

func deadEndExit(c journal.DeadEndCondition) int {
	switch c {
	case journal.DeadEndFailureAtZero:
		return exitFailureAtZero
	case journal.DeadEndSMU:
		return exitSMU
	case journal.DeadEndNoEvidence:
		return exitNoEvidence
	case journal.DeadEndBootLoop:
		return exitBootLoop
	case journal.DeadEndContainment:
		return exitContainment
	case journal.DeadEndPreflight:
		return exitPreflight
	case journal.DeadEndDefect:
		return exitDefect
	}
	return exitError
}
