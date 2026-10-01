package journal

import (
	"bufio"
	"bytes"
	"compress/gzip"
	"os"
	"path/filepath"
	"testing"
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
	f.Add(data, Schema)
	f.Add(data[:len(data)-5], Schema)
	f.Add([]byte{}, Schema)
	f.Fuzz(func(t *testing.T, data []byte, schema int) {
		if schema != 1 && schema != Schema {
			return
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
	header := []byte("{\"seq\":1,\"kind\":\"session.start\",\"schema\":2}\n")
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
			f, err := os.Open(filepath.Join("..", "carry", "testdata", tt.session+".jsonl.gz"))
			if err != nil {
				t.Fatal(err)
			}
			defer f.Close()
			z, err := gzip.NewReader(f)
			if err != nil {
				t.Fatal(err)
			}
			defer z.Close()
			reader := bufio.NewReader(z)
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
