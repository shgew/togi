package tuner

import "github.com/shgew/togi/internal/machine"

type CrashKind int

const (
	CrashInTrial CrashKind = iota
	CrashIdle
	CrashStray
	CrashInconclusive
	CrashThermal
)

type CrashFacts struct {
	InTrial, Applied, Evidence bool
	Reason                     machine.ResetReason
	Confirmed                  bool
}

func ClassifyCrash(f CrashFacts) CrashKind {
	ordinary := func() CrashKind {
		if f.InTrial {
			return CrashInTrial
		}
		if f.Applied {
			return CrashIdle
		}
		return CrashStray
	}
	if f.Evidence {
		return ordinary()
	}
	switch f.Reason.Kind {
	case machine.ResetThermalTrip:
		return CrashThermal
	case machine.ResetPowerButton:
		if f.InTrial {
			return CrashInTrial
		}
		return CrashInconclusive
	case "":
		if f.Reason.Supported && f.Confirmed {
			return CrashInconclusive
		}
	case machine.ResetWatchdog, machine.ResetSyncFlood, machine.ResetCPUShutdown, machine.ResetPowerLoss, machine.ResetUnknown:
	}
	return ordinary()
}
