package machine

type PMTable struct {
	PowerW          [16]float32 `json:"power_w"`
	VoltageRequestV [16]float32 `json:"voltage_request_v"`
	TemperatureC    [16]float32 `json:"temperature_c"`
	C0Pct           [16]float32 `json:"c0_pct"`
	CC1Pct          [16]float32 `json:"cc1_pct"`
	CC6Pct          [16]float32 `json:"cc6_pct"`
}

type ConditionsReader interface {
	PMTable() *PMTable
}
