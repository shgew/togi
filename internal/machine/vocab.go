package machine

type Condition string

const (
	Isolated Condition = "isolated"
	Resident Condition = "resident"
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
)

func (b BankType) CoreLocal() bool {
	switch b {
	case LoadStore, InstructionFetch, L2Cache, DecodeUnit, ExecutionUnit, FloatingPoint:
		return true
	case L3Cache, MemoryController, DataFabric:
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
