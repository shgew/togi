package machine

import "fmt"

type Condition string

const (
	Isolated Condition = "isolated"
	Resident Condition = "resident"
	Masked   Condition = "masked"
)

type Signal string

const (
	ComputationError Signal = "computation_error"
	UnexpectedExit   Signal = "unexpected_exit"
	Stall            Signal = "stall"
	CorrectedMCE     Signal = "corrected_mce"
	UncorrectedMCE   Signal = "uncorrected_mce"
	Crash            Signal = "crash"
)

type BankType string

const (
	LoadStore        BankType = "load_store"
	InstructionFetch BankType = "instruction_fetch"
	L2Cache          BankType = "l2_cache"
	DecodeUnit       BankType = "decode_unit"
	ExecutionUnit    BankType = "execution_unit"
	FloatingPoint    BankType = "floating_point"
	L3Cache          BankType = "l3_cache"
	MemoryController BankType = "unified_memory_controller"
	DataFabric       BankType = "data_fabric"
	// OtherBank is a bank the kernel decoded that is none of the named ones.
	OtherBank BankType = "other"
	// UnknownBank is a machine check without a decoded bank line.
	UnknownBank BankType = "unknown"
)

func (b BankType) CoreLocal() bool {
	switch b {
	case LoadStore, InstructionFetch, L2Cache, DecodeUnit, ExecutionUnit, FloatingPoint:
		return true
	case L3Cache, MemoryController, DataFabric, OtherBank, UnknownBank:
		return false
	}
	return false
}

type CoreInfo struct {
	Core int   `json:"core"`
	CCD  int   `json:"ccd"`
	CPUs []int `json:"cpus"`
}

type BIOSContext struct {
	BIOSVersion   string `json:"bios_version"`
	Board         string `json:"board"`
	CPUModel      string `json:"cpu_model"`
	Microcode     string `json:"microcode"`
	BoostLimitMHz int    `json:"boost_limit_mhz"`
}

func CompareContext(recorded, current BIOSContext) (detail string, same bool) {
	for _, f := range []struct {
		name            string
		recorded, found any
	}{
		{"bios_version", recorded.BIOSVersion, journalString(current.BIOSVersion)},
		{"board", recorded.Board, journalString(current.Board)},
		{"cpu_model", recorded.CPUModel, journalString(current.CPUModel)},
		{"microcode", recorded.Microcode, journalString(current.Microcode)},
		{"boost_limit_mhz", recorded.BoostLimitMHz, current.BoostLimitMHz},
	} {
		if f.recorded != f.found {
			return fmt.Sprintf("%s is %v; the session recorded %v", f.name, f.found, f.recorded), false
		}
	}
	return "matches the session", true
}

// journalString replaces each invalid UTF-8 byte with U+FFFD, as the journal's JSON encoding does to recorded values.
func journalString(s string) string {
	return string([]rune(s))
}
