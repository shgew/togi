package detect

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"os/exec"
	"regexp"
	"strconv"
	"strings"
	"time"

	"github.com/shgew/togi/internal/machine"
)

type Message struct {
	Time      time.Time
	Text      string
	Monotonic time.Duration
}

type Kernel struct {
	cpuCore    map[int]int
	journalctl func(args []string) (stdout, stderr []byte, exitCode int, err error)
}

func NewKernel(cores []machine.CoreInfo) *Kernel {
	cpuCore := make(map[int]int)
	for _, core := range cores {
		for _, cpu := range core.CPUs {
			cpuCore[cpu] = core.Core
		}
	}
	return &Kernel{cpuCore: cpuCore, journalctl: runJournalctl}
}

func runJournalctl(args []string) (stdout, stderr []byte, exitCode int, err error) {
	cmd := exec.Command("journalctl", args...)
	var errOut bytes.Buffer
	cmd.Stderr = &errOut
	out, err := cmd.Output()
	if exit, ok := errors.AsType[*exec.ExitError](err); ok && exit.Exited() {
		return out, errOut.Bytes(), exit.ExitCode(), nil
	}
	return out, errOut.Bytes(), 0, err
}

func (k *Kernel) MCEs(boot string, since time.Duration) ([]machine.MCE, error) {
	args := []string{"-k", "-b", strings.ReplaceAll(boot, "-", ""), "-o", "json", "--output-fields=MESSAGE,__REALTIME_TIMESTAMP,__MONOTONIC_TIMESTAMP", "--grep", "Hardware Error", "--no-pager", "-q"}
	out, stderr, code, err := k.journalctl(args)
	if err != nil {
		return nil, fmt.Errorf("read kernel log of boot %s: %w: %s", boot, err, bytes.TrimSpace(stderr))
	}
	if code != 0 {
		if code == 1 && (bytes.Contains(stderr, noBootEntry) || len(out) == 0 && len(stderr) == 0) {
			return nil, nil
		}
		return nil, fmt.Errorf("read kernel log of boot %s: exit status %d: %s", boot, code, bytes.TrimSpace(stderr))
	}

	var msgs []Message
	for line := range bytes.SplitSeq(out, []byte{'\n'}) {
		if len(line) == 0 {
			continue
		}
		var entry struct {
			Message   json.RawMessage `json:"MESSAGE"`
			Timestamp string          `json:"__REALTIME_TIMESTAMP"`
			Monotonic string          `json:"__MONOTONIC_TIMESTAMP"`
		}
		if err := json.Unmarshal(line, &entry); err != nil {
			return nil, fmt.Errorf("decode kernel log of boot %s: %w", boot, err)
		}
		var text string
		if err := json.Unmarshal(entry.Message, &text); err != nil {
			var raw []byte
			if err := json.Unmarshal(entry.Message, &raw); err != nil {
				return nil, fmt.Errorf("decode kernel message of boot %s: %w", boot, err)
			}
			text = string(raw)
		}
		micros, err := strconv.ParseInt(entry.Timestamp, 10, 64)
		if err != nil {
			return nil, fmt.Errorf("decode kernel timestamp of boot %s: %w", boot, err)
		}
		mono, err := strconv.ParseInt(entry.Monotonic, 10, 64)
		if err != nil {
			return nil, fmt.Errorf("decode kernel monotonic timestamp of boot %s: %w", boot, err)
		}
		if at := time.Duration(mono) * time.Microsecond; at >= since {
			msgs = append(msgs, Message{Time: time.UnixMicro(micros), Text: text, Monotonic: at})
		}
	}
	return Parse(msgs, k.cpuCore), nil
}

var noBootEntry = []byte("No journal boot entry found")
var resetLine = regexp.MustCompile(`^x86/amd: Previous system reset reason \[0x[0-9a-f]{8}\]: (.+)$`)
var versionLine = regexp.MustCompile(`^Linux version (\d+)\.(\d+)`)

func (k *Kernel) ResetReason(boot string) (machine.ResetReason, error) {
	args := []string{"-k", "-b", strings.ReplaceAll(boot, "-", ""), "-o", "json", "--output-fields=MESSAGE", "--grep", "Linux version|Previous system reset reason", "--no-pager", "-q"}
	out, stderr, code, err := k.journalctl(args)
	if err != nil {
		return machine.ResetReason{}, fmt.Errorf("read kernel log of boot %s: %w: %s", boot, err, bytes.TrimSpace(stderr))
	}
	if code != 0 {
		if code == 1 && bytes.Contains(stderr, noBootEntry) {
			return machine.ResetReason{}, fmt.Errorf("read kernel log of boot %s: %w", boot, machine.ErrBootMissing)
		}
		if code == 1 && len(out) == 0 && len(stderr) == 0 {
			return machine.ResetReason{}, nil
		}
		return machine.ResetReason{}, fmt.Errorf("read kernel log of boot %s: exit status %d: %s", boot, code, bytes.TrimSpace(stderr))
	}
	var reason machine.ResetReason
	var raw []string
	priority := map[machine.ResetKind]int{machine.ResetUnknown: 1, machine.ResetPowerButton: 2, machine.ResetCPUShutdown: 3, machine.ResetSyncFlood: 4, machine.ResetWatchdog: 5, machine.ResetThermalTrip: 6}
	for line := range bytes.SplitSeq(out, []byte{'\n'}) {
		if len(line) == 0 {
			continue
		}
		var entry struct {
			Message json.RawMessage `json:"MESSAGE"`
		}
		if err := json.Unmarshal(line, &entry); err != nil {
			return machine.ResetReason{}, fmt.Errorf("decode kernel log of boot %s: %w", boot, err)
		}
		var text string
		if err := json.Unmarshal(entry.Message, &text); err != nil {
			var data []byte
			if err := json.Unmarshal(entry.Message, &data); err != nil {
				return machine.ResetReason{}, fmt.Errorf("decode kernel message of boot %s: %w", boot, err)
			}
			text = string(data)
		}
		if match := versionLine.FindStringSubmatch(text); match != nil {
			major, _ := strconv.Atoi(match[1])
			minor, _ := strconv.Atoi(match[2])
			reason.Supported = reason.Supported || major > 6 || major == 6 && minor >= 16
		}
		if match := resetLine.FindStringSubmatch(text); match != nil {
			raw = append(raw, match[1])
			kind := resetKind(match[1])
			if priority[kind] > priority[reason.Kind] {
				reason.Kind = kind
			}
		}
	}
	reason.Raw = strings.Join(raw, "\n")
	return reason, nil
}

func resetKind(text string) machine.ResetKind {
	switch text {
	case "hardware watchdog timer expired":
		return machine.ResetWatchdog
	case "an uncorrected error caused a data fabric sync flood event", "a software sync flood event occurred":
		return machine.ResetSyncFlood
	case "internal CPU shutdown event occurred":
		return machine.ResetCPUShutdown
	case "power button was pressed for 4 seconds":
		return machine.ResetPowerButton
	case "thermal pin BP_THERMTRIP_L was tripped", "internal CPU thermal limit was tripped":
		return machine.ResetThermalTrip
	default:
		return machine.ResetUnknown
	}
}

var (
	decodedStatus = regexp.MustCompile(`^\[Hardware Error\]: CPU:(\d+) \(([0-9a-f]+):([0-9a-f]+):([0-9a-f]+)\) MC(\d+)_STATUS\[([^\]]*)\]: 0x([0-9a-f]{16})$`)
	rawStatus     = regexp.MustCompile(`^\[Hardware Error\]: CPU (\d+): Machine Check( Exception)?: [0-9a-f]+ Bank (\d+): ([0-9a-f]{16})$`)
	bankLine      = regexp.MustCompile(`^\[Hardware Error\]: (.+) Ext\. Error Code: \d+`)
)

func description(text string) bool {
	switch text {
	case "[Hardware Error]: Corrected error, no action required.",
		"[Hardware Error]: Deferred error, no action required.",
		"[Hardware Error]: Uncorrected, software restartable error.",
		"[Hardware Error]: Uncorrected, software containable error.",
		"[Hardware Error]: System Fatal error.":
		return true
	}
	return false
}

func bankType(name string) machine.BankType {
	switch name {
	case "Load Store Unit":
		return machine.LoadStore
	case "Instruction Fetch Unit":
		return machine.InstructionFetch
	case "L2 Cache":
		return machine.L2Cache
	case "Decode Unit":
		return machine.DecodeUnit
	case "Execution Unit":
		return machine.ExecutionUnit
	case "Floating Point Unit":
		return machine.FloatingPoint
	case "L3 Cache":
		return machine.L3Cache
	case "Unified Memory Controller", "Unified Memory Controller v2":
		return machine.MemoryController
	case "Coherent Station", "Power, Interrupts, etc.":
		return machine.DataFabric
	default:
		return machine.OtherBank
	}
}

func stamped(msg Message) string {
	return fmt.Sprintf("%d.%06d %s", msg.Time.Unix(), msg.Time.Nanosecond()/1000, msg.Text)
}

func Parse(msgs []Message, cpuCore map[int]int) []machine.MCE {
	var result []machine.MCE
	var preceding *Message
	current := -1
	statusLines := 0
	rawBlock := false
	pending := 0
	ambiguous := false
	for _, msg := range msgs {
		text := msg.Text
		if description(text) {
			preceding = &msg
			current = -1
			rawBlock = false
			pending = 0
			ambiguous = false
			continue
		}
		decoded := decodedStatus.FindStringSubmatch(text)
		raw := rawStatus.FindStringSubmatch(text)
		if decoded != nil || raw != nil {
			var cpu, bank int
			var corrected bool
			if decoded != nil {
				cpu, _ = strconv.Atoi(decoded[1])
				bank, _ = strconv.Atoi(decoded[5])
				fields := strings.Split(decoded[6], "|")
				corrected = len(fields) > 1 && fields[1] == "CE"
			} else {
				cpu, _ = strconv.Atoi(raw[1])
				bank, _ = strconv.Atoi(raw[3])
				status, _ := strconv.ParseUint(raw[4], 16, 64)
				corrected = status&(1<<61|1<<44) == 0
			}
			core, ok := cpuCore[cpu]
			if !ok {
				core = -1
			}
			if decoded != nil {
				ambiguous = pending != 0
				pending++
			}
			mce := machine.MCE{CPU: cpu, Core: core, Bank: bank, BankType: machine.UnknownBank, Corrected: corrected, Time: msg.Time, Monotonic: msg.Monotonic}
			if preceding != nil {
				mce.Lines = append(mce.Lines, stamped(*preceding))
			}
			mce.Lines = append(mce.Lines, stamped(msg))
			statusLines = len(mce.Lines)
			result = append(result, mce)
			current = len(result) - 1
			rawBlock = raw != nil
			preceding = nil
			continue
		}
		preceding = nil
		if !strings.HasPrefix(text, "[Hardware Error]: ") {
			current = -1
			continue
		}
		if current == -1 {
			continue
		}
		mce := &result[current]
		match := bankLine.FindStringSubmatch(text)
		reserved := strings.HasPrefix(text, "[Hardware Error]: Bank ") && strings.HasSuffix(text, " is reserved.")
		if match == nil && !reserved {
			if !ambiguous {
				mce.Lines = append(mce.Lines, stamped(msg))
			}
			continue
		}
		if !rawBlock && pending == 0 {
			ambiguous = true
			mce.BankType = machine.UnknownBank
			mce.Lines = mce.Lines[:statusLines]
		}
		if !ambiguous && (rawBlock || pending == 1) {
			if mce.Core != -1 && !rawBlock {
				if match != nil {
					mce.BankType = bankType(match[1])
				} else {
					mce.BankType = machine.OtherBank
				}
			}
			mce.Lines = append(mce.Lines, stamped(msg))
		}
		if pending > 0 {
			pending--
		}
	}
	return result
}
