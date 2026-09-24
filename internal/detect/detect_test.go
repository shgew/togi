package detect

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"code.marleb.org/shgew/shycler/internal/machine"
)

func loadFixture(t *testing.T, name string) []Message {
	t.Helper()
	b, err := os.ReadFile(filepath.Join("testdata", name))
	if err != nil {
		t.Fatal(err)
	}
	var msgs []Message
	for _, line := range strings.Split(strings.TrimSpace(string(b)), "\n") {
		var entry struct {
			Message   string `json:"MESSAGE"`
			Timestamp string `json:"__REALTIME_TIMESTAMP"`
		}
		if err := json.Unmarshal([]byte(line), &entry); err != nil {
			t.Fatal(err)
		}
		var micros int64
		if _, err := fmt.Sscan(entry.Timestamp, &micros); err != nil {
			t.Fatal(err)
		}
		msgs = append(msgs, Message{Time: time.UnixMicro(micros), Text: entry.Message})
	}
	return msgs
}

func TestCapturedBlocks(t *testing.T) {
	for _, tt := range []struct {
		fixture string
		cpus    []int
		bank    machine.BankType
	}{
		{"zen4-decode-unit.jsonl", []int{0, 8, 13}, machine.DecodeUnit},
		{"zen5-load-store.jsonl", []int{9}, machine.LoadStore},
	} {
		t.Run(tt.fixture, func(t *testing.T) {
			msgs := loadFixture(t, tt.fixture)
			cores := []machine.CoreInfo{{Core: 0, CPUs: []int{0, 16}}, {Core: 8, CPUs: []int{8, 24}}, {Core: 9, CPUs: []int{9, 25}}, {Core: 13, CPUs: []int{13, 29}}}
			got := Parse(msgs, NewKernel(cores).cpuCore)
			if len(got) != len(tt.cpus) {
				t.Fatalf("got %d errors, want %d", len(got), len(tt.cpus))
			}
			for i, mce := range got {
				if mce.CPU != tt.cpus[i] || mce.Core != tt.cpus[i] || mce.BankType != tt.bank || !mce.Corrected || !mce.BankType.CoreLocal() {
					t.Errorf("block %d: %+v", i, mce)
				}
				if mce.Time != msgs[indexStatus(msgs, i)].Time {
					t.Errorf("block %d: status timestamp mismatch", i)
				}
				if len(mce.Lines) != len(msgs)/len(tt.cpus)-1 {
					t.Errorf("block %d: lines = %v", i, mce.Lines)
				}
				if !strings.HasPrefix(mce.Lines[0], fmt.Sprintf("%d.%06d ", msgs[indexStatus(msgs, i)-1].Time.Unix(), msgs[indexStatus(msgs, i)-1].Time.Nanosecond()/1000)) {
					t.Errorf("block %d: incorrect timestamp on description: %v", i, mce.Lines[0])
				}
			}
		})
	}
}

func indexStatus(msgs []Message, block int) int {
	for i, msg := range msgs {
		if decodedStatus.MatchString(msg.Text) {
			if block == 0 {
				return i
			}
			block--
		}
	}
	return -1
}

func TestSMCALongNames(t *testing.T) {
	tests := []struct {
		name string
		bank machine.BankType
	}{
		{"Coherent Station", machine.DataFabric},
		{"DACC Back-end Unit", machine.OtherBank},
		{"DACC Front-end Unit", machine.OtherBank},
		{"Decode Unit", machine.DecodeUnit},
		{"eDDR5 CMN Unit", machine.OtherBank},
		{"Execution Unit", machine.ExecutionUnit},
		{"Floating Point Unit", machine.FloatingPoint},
		{"Global Memory Interconnect PCS Unit", machine.OtherBank},
		{"Global Memory Interconnect PHY Unit", machine.OtherBank},
		{"Instruction Fetch Unit", machine.InstructionFetch},
		{"L2 Cache", machine.L2Cache},
		{"L3 Cache", machine.L3Cache},
		{"Load Store Unit", machine.LoadStore},
		{"Microprocessor 5 Unit", machine.OtherBank},
		{"MPART Unit", machine.OtherBank},
		{"MPASP Unit", machine.OtherBank},
		{"MPDACC Unit", machine.OtherBank},
		{"MPDMA Unit", machine.OtherBank},
		{"MPM Unit", machine.OtherBank},
		{"MPRAS Unit", machine.OtherBank},
		{"NBIF Unit", machine.OtherBank},
		{"Northbridge IO Unit", machine.OtherBank},
		{"Parameter Block", machine.OtherBank},
		{"PCI Express Unit", machine.OtherBank},
		{"PCIe Link Unit", machine.OtherBank},
		{"Power, Interrupts, etc.", machine.DataFabric},
		{"Platform Security Processor", machine.OtherBank},
		{"Reserved", machine.OtherBank},
		{"SATA Unit", machine.OtherBank},
		{"System Hub Unit", machine.OtherBank},
		{"System Management Unit", machine.OtherBank},
		{"Die to Die Interconnect Unit", machine.OtherBank},
		{"Unified Memory Controller", machine.MemoryController},
		{"Unified Memory Controller v2", machine.MemoryController},
		{"USB Unit", machine.OtherBank},
		{"WAFL PHY Unit", machine.OtherBank},
		{"Ext Global Memory Interconnect PCS Unit", machine.OtherBank},
		{"Ext Global Memory Interconnect PHY Unit", machine.OtherBank},
	}
	fixture := loadFixture(t, "zen4-decode-unit.jsonl")[:6]
	coreLocal := map[string]bool{
		"Load Store Unit": true, "Instruction Fetch Unit": true, "L2 Cache": true,
		"Decode Unit": true, "Execution Unit": true, "Floating Point Unit": true,
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			msgs := append([]Message(nil), fixture...)
			msgs[4].Text = "[Hardware Error]: " + tt.name + " Ext. Error Code: 1"
			got := Parse(msgs, map[int]int{0: 0})
			if len(got) != 1 || got[0].BankType != tt.bank || got[0].BankType.CoreLocal() != coreLocal[tt.name] {
				t.Fatalf("bank %s: %+v", tt.name, got)
			}
		})
	}
}

func TestStatusAndBlockBoundaries(t *testing.T) {
	at := time.Unix(100, 13_000)
	for _, tt := range []struct {
		name      string
		status    string
		corrected bool
	}{
		{"decoded corrected", "[Hardware Error]: CPU:12 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135", true},
		{"decoded uncorrected", "[Hardware Error]: CPU:12 (1a:44:0) MC1_STATUS[Over|UE|-]: 0xbc00000000010135", false},
		{"decoded deferred", "[Hardware Error]: CPU:12 (1a:44:0) MC1_STATUS[Over|-|-]: 0xbc00000000010135", false},
		{"raw corrected", "[Hardware Error]: CPU 12: Machine Check: 0 Bank 1: 0000000000000000", true},
		{"raw uncorrected", "[Hardware Error]: CPU 12: Machine Check Exception: 0 Bank 1: 2000000000000000", false},
		{"raw deferred", "[Hardware Error]: CPU 12: Machine Check: 0 Bank 1: 0000100000000000", false},
	} {
		t.Run(tt.name, func(t *testing.T) {
			msgs := []Message{{at, "[Hardware Error]: Corrected error, no action required."}, {at.Add(time.Microsecond), tt.status}, {at.Add(2 * time.Microsecond), "[Hardware Error]: Load Store Unit Ext. Error Code: 13"}}
			got := Parse(msgs, map[int]int{12: 6})
			bank := machine.LoadStore
			if strings.HasPrefix(tt.name, "raw") {
				bank = machine.UnknownBank
			}
			if len(got) != 1 || got[0].Corrected != tt.corrected || got[0].BankType != bank || got[0].Core != 6 || got[0].Bank != 1 || !reflect.DeepEqual(got[0].Lines, []string{stamped(msgs[0]), stamped(msgs[1]), stamped(msgs[2])}) {
				t.Fatalf("unexpected block: %+v", got)
			}
		})
	}
	msgs := []Message{{at, "[Hardware Error]: CPU:99 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135"}, {at, "[Hardware Error]: Bank 1 is reserved."}, {at, "unrelated"}, {at, "[Hardware Error]: Decode Unit Ext. Error Code: 1"}, {at, "[Hardware Error]: Deferred error, no action required."}, {at, "[Hardware Error]: CPU:12 (1a:44:0) MC1_STATUS[Over|UE|-]: 0xbc00000000010135"}}
	got := Parse(msgs, map[int]int{12: 6})
	if len(got) != 2 || got[0].Core != -1 || got[0].BankType != machine.UnknownBank || len(got[0].Lines) != 2 || got[1].Core != 6 || got[1].Corrected || len(got[1].Lines) != 2 {
		t.Fatalf("boundary/unknown CPU: %+v", got)
	}
	msgs[0].Text = "[Hardware Error]: CPU:12 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135"
	got = Parse(msgs[:2], map[int]int{12: 6})
	if got[0].BankType != machine.OtherBank {
		t.Fatalf("reserved bank: %+v", got)
	}
}

func TestInterleavedBankAttribution(t *testing.T) {
	statuses := map[int]string{
		0: "[Hardware Error]: CPU:0 (1a:44:0) MC3_STATUS[Over|CE|-]: 0xbc00000000010135",
		9: "[Hardware Error]: CPU:9 (1a:44:0) MC0_STATUS[Over|CE|-]: 0xbc00000000010135",
		3: "[Hardware Error]: CPU 3: Machine Check: 0 Bank 5: bea0000000000108",
	}
	banks := map[int]string{
		0: "[Hardware Error]: Decode Unit Ext. Error Code: 1",
		9: "[Hardware Error]: Load Store Unit Ext. Error Code: 13",
	}
	for _, tt := range []struct {
		name   string
		order  []int
		lines  []int
		marker bool
		want   map[int]machine.BankType
	}{
		{"CPU 0 then CPU 9", []int{0, 9}, []int{0, 9}, false, map[int]machine.BankType{0: machine.UnknownBank, 9: machine.UnknownBank}},
		{"CPU 9 then CPU 0 with banner", []int{9, 0}, []int{9, 0}, true, map[int]machine.BankType{0: machine.UnknownBank, 9: machine.UnknownBank}},
		{"sequential records", []int{0, 9}, nil, false, map[int]machine.BankType{0: machine.DecodeUnit, 9: machine.LoadStore}},
		{"raw record then decoded", []int{3, 0}, nil, false, map[int]machine.BankType{3: machine.UnknownBank, 0: machine.DecodeUnit}},
	} {
		t.Run(tt.name, func(t *testing.T) {
			var msgs []Message
			add := func(text string) {
				msgs = append(msgs, Message{Time: time.UnixMicro(int64(len(msgs))), Text: text})
			}
			for i, cpu := range tt.order {
				add("[Hardware Error]: Corrected error, no action required.")
				add(statuses[cpu])
				if tt.marker && i == 0 {
					add("mce: [Hardware Error]: Machine check events logged")
				}
				if tt.lines == nil {
					if bank, ok := banks[cpu]; ok {
						add(bank)
					}
				} else if i == len(tt.order)-1 {
					for _, owner := range tt.lines {
						add(banks[owner])
					}
				}
			}
			got := Parse(msgs, map[int]int{0: 0, 3: 3, 9: 9})
			if len(got) != len(tt.order) {
				t.Fatalf("got %d MCEs, want %d", len(got), len(tt.order))
			}
			for _, mce := range got {
				if mce.BankType != tt.want[mce.CPU] {
					t.Errorf("CPU %d bank type = %v, want %v", mce.CPU, mce.BankType, tt.want[mce.CPU])
				}
			}
		})
	}
}

func TestJournalBootAndByteArray(t *testing.T) {
	bin := t.TempDir()
	path := filepath.Join(bin, "journalctl")
	argsPath := filepath.Join(bin, "args")
	script := "#!/bin/sh\nprintf '%s\\n' \"$@\" > " + argsPath + "\ncase \"$*\" in\n  *unknown*) echo 'No journal boot entry found' >&2; exit 1;;\n  *empty*) exit 1;;\n  *broken*) echo 'permission denied' >&2; exit 2;;\nesac\nprintf '%s\\n' '{\"MESSAGE\":[91,72,97,114,100,119,97,114,101,32,69,114,114,111,114,93,58,32,67,80,85,58,50,32,40,49,97,58,52,52,58,48,41,32,77,67,48,95,83,84,65,84,85,83,91,79,118,101,114,124,67,69,93,58,32,48,120,100,99,50,48,52,48,48,48,48,48,48,100,48,49,55,53],\"__REALTIME_TIMESTAMP\":\"2000000\"}'\n"
	if err := os.WriteFile(path, []byte(script), 0755); err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", bin+":"+os.Getenv("PATH"))
	kernel := NewKernel([]machine.CoreInfo{{Core: 1, CPUs: []int{2, 18}}})
	got, err := kernel.MCEs("1234-abcd", time.Unix(1, 0))
	if err != nil || len(got) != 1 || got[0].Core != 1 || got[0].Time != time.Unix(2, 0) {
		t.Fatalf("byte-array journal entry: %+v, %v", got, err)
	}
	args, err := os.ReadFile(argsPath)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(args), "1234abcd\n") || !strings.Contains(string(args), "--since\n@1\n") {
		t.Fatalf("journalctl arguments: %s", args)
	}
	got, err = kernel.MCEs("unknown", time.Time{})
	if err != nil || got != nil {
		t.Fatalf("unknown boot: %+v, %v", got, err)
	}
	got, err = kernel.MCEs("empty", time.Time{})
	if err != nil || got != nil {
		t.Fatalf("no matches: %+v, %v", got, err)
	}
	if _, err := kernel.MCEs("broken", time.Time{}); err == nil || !strings.Contains(err.Error(), "read kernel log of boot broken:") || !strings.Contains(err.Error(), "permission denied") {
		t.Fatalf("journal failure: %v", err)
	}
}
