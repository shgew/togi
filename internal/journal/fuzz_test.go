package journal

import (
	"bufio"
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func FuzzParse(f *testing.F) {
	dir := f.TempDir()
	j := openTest(f, dir)
	appendAll(f, j, samplePayloads())
	j.Close()
	data, err := os.ReadFile(filepath.Join(dir, eventsFile))
	if err != nil {
		f.Fatal(err)
	}
	f.Add(data, false)
	f.Add(data[:len(data)-5], false)
	f.Add([]byte{}, false)
	f.Fuzz(func(t *testing.T, data []byte, legacy bool) {
		schema := Schema
		if legacy {
			schema = 1
		}
		events, end, err := parse(data, Build{Schema: schema})
		if err != nil {
			return
		}
		if end < 0 || end > len(data) {
			t.Fatalf("end %d outside [0, %d]", end, len(data))
		}
		if end > 0 && data[end-1] != '\n' {
			t.Fatalf("accepted prefix ends inside a line at %d", end)
		}
		rawEnd := 0
		for i, event := range events {
			if event.Seq != i+1 {
				t.Fatalf("event %d has sequence %d", i+1, event.Seq)
			}
			next := rawEnd + len(event.Raw) + 1
			if next > end || data[next-1] != '\n' || !bytes.Equal(event.Raw, data[rawEnd:next-1]) {
				t.Fatalf("raw event %d differs from input at %d", i+1, rawEnd)
			}
			rawEnd = next
		}
		if rawEnd != end {
			t.Fatalf("raw events cover %d bytes, accepted prefix covers %d", rawEnd, end)
		}
	})
}

func TestParseInputBoundaries(t *testing.T) {
	header := []byte(fmt.Sprintf(`{"seq":1,"kind":"session.start","schema":%d}`+"\n", Schema))
	unknown := []byte("{\"seq\":2,\"kind\":\"future.observation\",\"evidence\":17}\n")
	for _, tt := range []struct {
		name string
		tail []byte
		end  int
		seqs int
		fail bool
	}{
		{"event boundary", nil, len(header), 1, false},
		{"unknown kind", unknown, len(header) + len(unknown), 2, false},
		{"truncated event", unknown[:len(unknown)-4], len(header), 1, false},
		{"missing final newline", unknown[:len(unknown)-1], len(header), 1, false},
		{"complete malformed line", []byte("{\"seq\":2,\"kind\":\n"), 0, 0, true},
		{"sequence gap", []byte("{\"seq\":3,\"kind\":\"future.observation\"}\n"), 0, 0, true},
	} {
		t.Run(tt.name, func(t *testing.T) {
			data := bytes.Join([][]byte{header, tt.tail}, nil)
			events, end, err := parse(data, Build{Schema: Schema})
			if (err != nil) != tt.fail {
				t.Fatalf("parse error %v, want failure %t", err, tt.fail)
			}
			if end != tt.end || len(events) != tt.seqs {
				t.Fatalf("accepted end %d with %d events, want end %d with %d events", end, len(events), tt.end, tt.seqs)
			}
			if !tt.fail && !bytes.Equal(events[0].Raw, header[:len(header)-1]) {
				t.Fatalf("first raw event %q, want %q", events[0].Raw, header[:len(header)-1])
			}
			if tt.name == "unknown kind" && (events[1].Kind != "future.observation" || events[1].Data != nil || !bytes.Equal(events[1].Raw, unknown[:len(unknown)-1])) {
				t.Fatalf("unknown event not preserved as opaque raw evidence: %+v", events[1])
			}
		})
	}
}

func TestParseShippedSchemas(t *testing.T) {
	for _, tt := range []struct {
		session string
		schema  int
	}{
		{"20260924T204352Z", 1},
		{"20260927T221954Z", 2},
	} {
		t.Run(tt.session, func(t *testing.T) {
			f, err := os.Open(filepath.Join("testdata", tt.session+"-config.jsonl"))
			if err != nil {
				t.Fatal(err)
			}
			defer f.Close()
			reader := bufio.NewReader(f)
			var prefix []byte
			for range 2 {
				line, err := reader.ReadBytes('\n')
				if err != nil {
					t.Fatal(err)
				}
				prefix = append(prefix, line...)
			}
			events, end, err := parse(prefix, Build{Schema: tt.schema})
			if err != nil {
				t.Fatal(err)
			}
			if end != len(prefix) || len(events) != 2 {
				t.Fatalf("accepted %d bytes and %d events, want %d bytes and 2 events", end, len(events), len(prefix))
			}
			start, ok := events[0].Data.(*SessionStart)
			if !ok || start.Schema != tt.schema || start.Session != tt.session {
				t.Fatalf("shipped session header not decoded: %+v", events[0])
			}
			if _, ok := events[1].Data.(*ConfigLoaded); !ok {
				t.Fatalf("shipped configuration not decoded: %+v", events[1])
			}
		})
	}
}

func TestDamagedJournalReaders(t *testing.T) {
	header := fmt.Sprintf(`{"seq":1,"kind":"session.start","schema":%d,"session":"source"}`+"\n", Schema)
	for _, tc := range []struct{ name, data, diagnostic string }{
		{"malformed first line", "{\n", "line 1"},
		{"missing kind", header + `{"seq":2}` + "\n", "event has no kind"},
		{"wrong first event", fmt.Sprintf(`{"seq":1,"kind":"shutdown","schema":%d}`+"\n", Schema), "first event is shutdown"},
		{"invalid payload", header + `{"seq":2,"kind":"trial.end","duration_s":"bad"}` + "\n", "decode trial.end"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			dir := t.TempDir()
			path := filepath.Join(dir, eventsFile)
			data := []byte(tc.data)
			if err := os.WriteFile(path, data, 0600); err != nil {
				t.Fatal(err)
			}
			for _, reader := range []func(string) ([]Event, error){ReadHistory, ReadForCarry} {
				_, err := reader(path)
				if err == nil || !strings.Contains(err.Error(), tc.diagnostic) {
					t.Fatalf("reader error = %v, want %q", err, tc.diagnostic)
				}
			}
			_, _, err := Read(dir)
			if err == nil || !strings.Contains(err.Error(), tc.diagnostic) {
				t.Fatalf("Read error = %v, want %q", err, tc.diagnostic)
			}
			j, err := Open(dir, Options{Now: fixedClock()})
			if j != nil {
				j.Close()
			}
			if err == nil || !strings.Contains(err.Error(), tc.diagnostic) {
				t.Fatalf("Open error = %v, want %q", err, tc.diagnostic)
			}
			after, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			if diff := cmp.Diff(data, after); diff != "" {
				t.Fatalf("corrupt complete line was repaired: %s", diff)
			}
		})
	}
}

func TestHistoricalReadersRejectUnshippedSchemas(t *testing.T) {
	for _, schema := range []string{"-1", "0", fmt.Sprint(Schema + 1), "99"} {
		t.Run(schema, func(t *testing.T) {
			path := filepath.Join(t.TempDir(), eventsFile)
			if err := os.WriteFile(path, []byte(`{"seq":1,"kind":"session.start","schema":`+schema+`}`+"\n"), 0600); err != nil {
				t.Fatal(err)
			}
			for _, reader := range []func(string) ([]Event, error){ReadHistory, ReadForCarry} {
				if _, err := reader(path); err == nil || !strings.Contains(err.Error(), "cannot be read by schema") {
					t.Fatalf("schema refusal: %v", err)
				}
			}
		})
	}
}

func TestCarryAndHistoryKeepHistoricalBuildStamp(t *testing.T) {
	path := filepath.Join(t.TempDir(), eventsFile)
	data := `{"seq":1,"kind":"session.start","session":"source","schema":1}` + "\n" +
		`{"seq":2,"kind":"unrelated.future","nested":{"value":42}}` + "\n" +
		`{"seq":3,"kind":"config.loaded","version":"recorded","fixes":4,"config":"obsolete shape"}` + "\n" + `{"seq":4`
	if err := os.WriteFile(path, []byte(data), 0600); err != nil {
		t.Fatal(err)
	}
	for _, reader := range []func(string) ([]Event, error){ReadHistory, ReadForCarry} {
		events, err := reader(path)
		if err != nil {
			t.Fatal(err)
		}
		last := events[len(events)-1]
		if last.Seq != 3 {
			t.Fatalf("historical sequence rewritten: %d", last.Seq)
		}
		if diff := cmp.Diff(&ConfigLoaded{Version: "recorded", Fixes: 4}, last.Data); diff != "" {
			t.Fatal(diff)
		}
	}
}

func TestCarryRejectsEmptyOrInvalidStamp(t *testing.T) {
	for _, data := range []string{"", `{"kind":"session.start"}`, `{"kind":"session.start"}` + "\n" + `{"kind":"config.loaded","version":17}` + "\n"} {
		path := filepath.Join(t.TempDir(), eventsFile)
		if err := os.WriteFile(path, []byte(data), 0600); err != nil {
			t.Fatal(err)
		}
		if _, err := ReadForCarry(path); err == nil {
			t.Fatalf("invalid carry journal accepted: %q", data)
		}
	}
}
