// Package tuningboot persists bounded retry state and leave reasons in GRUB.
package tuningboot

import (
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"strconv"
	"strings"
)

const (
	CountVariable  = "togi_restart_count"
	ReasonVariable = "togi_leave_reason"
	RetryMarker    = "/run/togi-retry-tuning-boot"

	retryReason     = "service restart limit hit; rebooting back into the tuning boot"
	exhaustedReason = "service restart limit exhausted; returning to the normal system"
	maxRecordBytes  = 300
)

type Environment interface {
	Get(name string) (string, error)
	Set(values map[string]string) error
	Unset(names ...string) error
}

type Bootloader interface {
	Environment
	ClearSavedEntry() (before, after string, err error)
}

type Reason struct {
	ID     string `json:"id"`
	Count  int    `json:"count"`
	Reason string `json:"reason"`
}

func NewReason(reason string, count int) (Reason, error) {
	var id [16]byte
	if _, err := rand.Read(id[:]); err != nil {
		return Reason{}, fmt.Errorf("create leave reason ID: %w", err)
	}
	r := Reason{ID: hex.EncodeToString(id[:]), Count: count, Reason: sanitizeReason(reason)}
	if err := validateReason(r); err != nil {
		return Reason{}, err
	}
	return r, nil
}

func ReadReason(env Environment) (*Reason, error) {
	value, err := env.Get(ReasonVariable)
	if err != nil {
		return nil, fmt.Errorf("read leave reason: %w", err)
	}
	if value == "" {
		return nil, nil
	}
	if len(value) > maxRecordBytes || !printableASCII(value) {
		return nil, fmt.Errorf("leave reason record must be printable ASCII and at most %d bytes", maxRecordBytes)
	}
	var record struct {
		ID     *string `json:"id"`
		Count  *int    `json:"count"`
		Reason *string `json:"reason"`
	}
	decoder := json.NewDecoder(strings.NewReader(value))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&record); err != nil {
		return nil, fmt.Errorf("decode leave reason: %w", err)
	}
	if err := decoder.Decode(new(any)); err != io.EOF {
		return nil, fmt.Errorf("leave reason record contains trailing data")
	}
	if record.ID == nil || record.Count == nil || record.Reason == nil {
		return nil, fmt.Errorf("leave reason record requires id, count and reason")
	}
	r := Reason{ID: *record.ID, Count: *record.Count, Reason: *record.Reason}
	if err := validateReason(r); err != nil {
		return nil, err
	}
	return &r, nil
}

func WriteReason(env Environment, reason Reason) error {
	value, err := encodeReason(reason)
	if err != nil {
		return err
	}
	if err := env.Set(map[string]string{ReasonVariable: value}); err != nil {
		return fmt.Errorf("write leave reason: %w", err)
	}
	return nil
}

func ClearReason(env Environment) error {
	if err := env.Unset(ReasonVariable); err != nil {
		return fmt.Errorf("clear leave reason: %w", err)
	}
	return nil
}

func ResetCount(env Environment) error {
	if err := env.Unset(CountVariable); err != nil {
		return fmt.Errorf("reset restart count: %w", err)
	}
	return nil
}

// RestartLimit records the attempt before requesting either boot action. A failed
// read or write leaves the command's caller responsible for its safe fallback.
func RestartLimit(bootloader Bootloader) (count int, retry bool, err error) {
	value, err := bootloader.Get(CountVariable)
	if err != nil {
		return 0, false, fmt.Errorf("read restart count: %w", err)
	}
	switch value {
	case "", "0":
		count = 1
	case "1":
		count = 2
	case "2", "3":
		count = 3
	default:
		return 0, false, fmt.Errorf("invalid restart count %q: must be 0 to 3", value)
	}
	retry = count < 3
	text := exhaustedReason
	if retry {
		text = retryReason
	}
	reason, err := NewReason(text, count)
	if err != nil {
		return count, false, err
	}
	encoded, err := encodeReason(reason)
	if err != nil {
		return count, false, err
	}
	if err := bootloader.Set(map[string]string{CountVariable: strconv.Itoa(count), ReasonVariable: encoded}); err != nil {
		return count, false, fmt.Errorf("write restart count and leave reason: %w", err)
	}
	if !retry {
		if _, _, err := bootloader.ClearSavedEntry(); err != nil {
			return count, false, fmt.Errorf("clear GRUB saved entry: %w", err)
		}
	}
	return count, retry, nil
}

func encodeReason(reason Reason) (string, error) {
	reason.Reason = sanitizeReason(reason.Reason)
	if err := validateReason(reason); err != nil {
		return "", err
	}
	// Budget the escaped reason before encoding, so the complete record fits
	// even when every character needs JSON escaping.
	budget := maxRecordBytes - len(`{"id":"","count":0,"reason":""}`) - len(reason.ID)
	end := 0
	for end < len(reason.Reason) {
		cost := 1
		switch reason.Reason[end] {
		case '"', '\\':
			cost = 2
		case '<', '>', '&':
			cost = 6
		}
		if cost > budget {
			break
		}
		budget -= cost
		end++
	}
	reason.Reason = reason.Reason[:end]
	data, err := json.Marshal(reason)
	if err != nil {
		return "", fmt.Errorf("encode leave reason: %w", err)
	}
	return string(data), nil
}

func sanitizeReason(reason string) string {
	if len(reason) <= 160 && printableASCII(reason) && strings.TrimSpace(reason) == reason {
		return reason
	}
	var b strings.Builder
	b.Grow(min(len(reason), 160))
	for _, c := range reason {
		if b.Len() == 160 {
			break
		}
		switch {
		case c == '\n' || c == '\r' || c == '\t':
			b.WriteByte(' ')
		case c < ' ' || c > '~':
			b.WriteByte('?')
		default:
			b.WriteByte(byte(c))
		}
	}
	return strings.TrimSpace(b.String())
}

func validateReason(reason Reason) error {
	if len(reason.ID) == 0 || len(reason.ID) > 64 {
		return fmt.Errorf("leave reason ID must have 1 to 64 safe bytes")
	}
	for i := range len(reason.ID) {
		c := reason.ID[i]
		if c != '-' && c != '_' && c != '.' && (c < '0' || c > '9') && (c < 'a' || c > 'z') && (c < 'A' || c > 'Z') {
			return fmt.Errorf("leave reason ID must contain only ASCII letters, digits, '.', '_' or '-'")
		}
	}
	if reason.Count < 0 || reason.Count > 3 {
		return fmt.Errorf("leave reason count must be 0 to 3")
	}
	if len(reason.Reason) == 0 || len(reason.Reason) > 160 || !printableASCII(reason.Reason) {
		return fmt.Errorf("leave reason must have 1 to 160 printable ASCII bytes")
	}
	return nil
}

func printableASCII(value string) bool {
	for i := range len(value) {
		if value[i] < ' ' || value[i] > '~' {
			return false
		}
	}
	return true
}
