package journal

import (
	"bytes"
	"encoding/json"
	"errors"
)

var legacyKinds = map[string]string{
	"guard.rotation": "checking.lap",
	"guard.step":     "checking.step",
	"hunt.mask":      "hunt.group",
	"mark.joint":     "combination",
	"refine.round":   "deepening.round",
}

var legacyFields = map[string]string{
	"rotation": "lap", "rotations": "laps",
	"anchor": "parked", "anchor_seq": "parked_seq",
	"mask": "group", "masks": "groups",
	"failed_mark": "failure_point", "check_edge": "check_solo_limit",
	"cleared_joint":   "cleared_combination",
	"candidate_edges": "candidate_solo_limits", "guard": "checking",
	"guard_trial_s": "checking_trial_s", "guard_idle_s": "checking_idle_s", "guard_all_core_s": "checking_all_core_s",
}

var legacyKindFields = map[string]map[string]string{
	"checking.lap":    {"clean": "passed", "qualifying": "full"},
	"hunt.group":      {"edge": "probe"},
	"combination":     {"mark": "combination"},
	"session.carried": {"marks": "failure_points", "edge": "candidate_solo_limit", "edge_session": "candidate_solo_limit_session", "edge_seq": "candidate_solo_limit_seq", "mark_session": "failure_point_session", "mark_seq": "failure_point_seq", "mark_signal": "failure_point_signal"},
	"deepening.round": {"anchor": "base", "anchor_seq": "base_seq"},
}

var legacyValues = map[string]map[string]string{
	"phase":     {"resident": "has_room", "done": "at_limit", "guard": "checking", "refine": "deepening"},
	"condition": {"isolated": "alone", "resident": "together", "masked": "parked"},
	"decision":  {"check_edge": "check_solo_limit"},
}

var legacyKindValues = map[string]map[string]map[string]string{
	"core.phase": {"from": legacyValues["phase"], "to": legacyValues["phase"]},
	"shutdown":   {"reason": {"rotations": "laps"}},
	"hunt.end":   {"result": {"joint": "combination"}},
	"hunt.group": {"stage": {"edge": "probe"}},
}

// translateLegacy changes the decoded vocabulary, not the recorded schema or provenance.
func translateLegacy(line []byte) ([]byte, error) {
	var body map[string]any
	decoder := json.NewDecoder(bytes.NewReader(line))
	decoder.UseNumber()
	if err := decoder.Decode(&body); err != nil {
		return nil, err
	}
	if len(bytes.TrimSpace(line[decoder.InputOffset():])) != 0 {
		return nil, errors.New("unexpected data after JSON event")
	}
	kind, _ := body["kind"].(string)
	if renamed := legacyKinds[kind]; renamed != "" {
		kind = renamed
		body["kind"] = kind
	}
	var translate func(any) any
	translate = func(value any) any {
		switch value := value.(type) {
		case map[string]any:
			translated := make(map[string]any, len(value))
			for field, child := range value {
				child = translate(child)
				if text, ok := child.(string); ok {
					if renamed := legacyValues[field][text]; renamed != "" {
						child = renamed
					}
					if renamed := legacyKindValues[kind][field][text]; renamed != "" {
						child = renamed
					}
				}
				name := legacyFields[field]
				if renamed := legacyKindFields[kind][field]; renamed != "" {
					name = renamed
				}
				if name == "" {
					name = field
				}
				translated[name] = child
			}
			return translated
		case []any:
			for i, child := range value {
				value[i] = translate(child)
			}
		}
		return value
	}
	return json.Marshal(translate(body))
}
