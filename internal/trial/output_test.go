package trial

import (
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/machine"
)

func TestOutputLineBoundaries(t *testing.T) {
	capLine := strings.Repeat("x", outputLineLimit)
	for _, tt := range []struct {
		name     string
		chunks   []string
		lines    []string
		pending  string
		exceeded bool
	}{
		{"exact unterminated", []string{capLine}, nil, capLine, false},
		{"exact newline", []string{capLine, "\n"}, []string{capLine}, "", false},
		{"exact carriage return", []string{capLine + "\r"}, []string{capLine}, "", false},
		{"chunk boundaries", []string{capLine[:4095], capLine[4095:], "\r", "\nnext\nlast"}, []string{capLine, "", "next"}, "last", false},
		{"separate capped lines", []string{capLine + "\n" + capLine + "\r"}, []string{capLine, capLine}, "", false},
		{"oversized unterminated", []string{capLine, "x"}, nil, capLine, true},
		{"oversized terminated", []string{"first\n" + capLine + "x\nCOMPUTE ERROR\n"}, []string{"first"}, capLine, true},
	} {
		t.Run(tt.name, func(t *testing.T) {
			var buffer outputLines
			var lines []string
			var err error
			for _, chunk := range tt.chunks {
				_, err = buffer.consume([]byte(chunk), func(line string) { lines = append(lines, line) })
				if err != nil {
					break
				}
			}
			if errors.Is(err, errOutputLineTooLong) != tt.exceeded || buffer.exceeded != tt.exceeded {
				t.Fatalf("exceeded=%t err=%v, want %t", buffer.exceeded, err, tt.exceeded)
			}
			if diff := cmp.Diff(tt.lines, lines); diff != "" {
				t.Fatalf("classified lines (-want +got):\n%s", diff)
			}
			if diff := cmp.Diff(tt.pending, string(buffer.pending)); diff != "" {
				t.Fatalf("pending prefix (-want +got):\n%s", diff)
			}
			if cap(buffer.pending) > outputLineLimit {
				t.Fatalf("retained allocation exceeds limit: %d", cap(buffer.pending))
			}
		})
	}
}

func TestWatchedOutputLargeAppend(t *testing.T) {
	path := filepath.Join(t.TempDir(), "results.txt")
	prefix := strings.Repeat("x", outputLineLimit-1)
	if err := os.WriteFile(path, []byte(prefix), 0644); err != nil {
		t.Fatal(err)
	}
	w := watchFile{path: path}
	var lines []string
	classify := func(line string) { lines = append(lines, line) }
	if err := w.read(classify); err != nil {
		t.Fatal(err)
	}
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND, 0644)
	if err != nil {
		t.Fatal(err)
	}
	_, err = io.WriteString(f, strings.Repeat("y", 4*1024*1024)+"\nCOMPUTE ERROR\n")
	f.Close()
	if err != nil {
		t.Fatal(err)
	}
	if err := w.read(classify); !errors.Is(err, errOutputLineTooLong) {
		t.Fatalf("large append error = %v", err)
	}
	if diff := cmp.Diff(prefix+"y", string(w.lines.pending)); diff != "" {
		t.Fatalf("retained prefix (-want +got):\n%s", diff)
	}
	if w.offset > outputLineLimit+4096 || len(lines) != 0 {
		t.Fatalf("read past oversized line: offset=%d lines=%q", w.offset, lines)
	}
	offset := w.offset
	if err := w.read(classify); err != nil || w.offset != offset || len(lines) != 0 {
		t.Fatalf("re-read rejected output: offset=%d lines=%q err=%v", w.offset, lines, err)
	}
}

type outputHost struct {
	*fakeHost
	text   string
	stderr bool
	watch  bool
}

func (h *outputHost) Start(ctx context.Context, _ []string, dir string) (process, error) {
	p, err := h.fakeHost.Start(ctx, []string{"fake", "sleep"}, dir)
	if err != nil {
		return nil, err
	}
	if h.watch {
		if err := os.WriteFile(filepath.Join(dir, "results.txt"), []byte(h.text), 0644); err != nil {
			p.(*fakeProc).finish(err)
			return nil, err
		}
	} else {
		out := p.(*fakeProc).stdoutW
		if h.stderr {
			out = p.(*fakeProc).stderrW
		}
		go func() { _, _ = io.WriteString(out, h.text) }()
	}
	return p, nil
}

func TestOversizedOutputWait(t *testing.T) {
	for _, source := range []string{"stdout", "stderr", "watched"} {
		t.Run(source, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, "watched")
				o.NoScope = false
				kills := 0
				h := &outputHost{fakeHost: &fakeHost{inScope: true, killScope: func(string) ([]byte, error) {
					kills++
					return nil, nil
				}}, text: "COMPUTE ERROR" + strings.Repeat("x", 1024*1024), stderr: source == "stderr", watch: source == "watched"}
				r := New(o)
				r.host = h
				started, err := r.Start(context.Background(), testSpec("oversized", machine.R1, time.Minute))
				if err != nil {
					t.Fatal(err)
				}
				trial := started.(*running)
				begin := time.Now()
				result, err := trial.Wait(context.Background(), &recorder{})
				if !errors.Is(err, errOutputLineTooLong) || errors.Is(err, machine.ErrContainment) || result.Inconclusive == "" || result.Signal != "" {
					t.Fatalf("oversized %s result=%+v err=%v", source, result, err)
				}
				if elapsed := time.Since(begin); elapsed > teardownLimit || kills != 1 {
					t.Fatalf("oversized teardown elapsed=%s scope kills=%d", elapsed, kills)
				}
				wantSignals := []fakeSignal{{1000, syscall.SIGCONT}, {1000, syscall.SIGTERM}, {1000, syscall.SIGKILL}}
				if diff := cmp.Diff(wantSignals, h.recordedSignals(), cmp.AllowUnexported(fakeSignal{})); diff != "" {
					t.Fatalf("teardown signals (-want +got):\n%s", diff)
				}
				select {
				case <-trial.instances[0].joined:
				default:
					t.Fatal("output readers or process waiter were not joined")
				}
				if source == "watched" {
					w := &trial.instances[0].watch[0]
					if diff := cmp.Diff(h.text[:outputLineLimit], string(w.lines.pending)); diff != "" {
						t.Fatalf("watched diagnostic prefix (-want +got):\n%s", diff)
					}
				} else {
					data, err := os.ReadFile(filepath.Join(o.Dir, "oversized", source+".log"))
					if err != nil {
						t.Fatal(err)
					}
					if diff := cmp.Diff(h.text[:outputLineLimit], string(data)); diff != "" {
						t.Fatalf("stream diagnostic prefix (-want +got):\n%s", diff)
					}
				}
			})
		})
	}
}

func TestExactOutputLineLimitWait(t *testing.T) {
	for _, source := range []string{"stdout", "stderr", "watched"} {
		for _, ending := range []string{"", "\n", "\r\n"} {
			t.Run(source+"/"+strings.ReplaceAll(strings.ReplaceAll(ending, "\r", "CR"), "\n", "LF"), func(t *testing.T) {
				synctest.Test(t, func(t *testing.T) {
					o := fakeOptions(t, "watched")
					message := "COMPUTE ERROR"
					h := &outputHost{fakeHost: &fakeHost{}, text: strings.Repeat("x", outputLineLimit-len(message)) + message + ending, stderr: source == "stderr", watch: source == "watched"}
					r := New(o)
					r.host = h
					trial, err := r.Start(context.Background(), testSpec("exact-cap", machine.R1, time.Second))
					if err != nil {
						t.Fatal(err)
					}
					var rec recorder
					result, err := trial.Wait(context.Background(), &rec)
					if err != nil || result.Signal != machine.ComputationError || result.Inconclusive != "" {
						t.Fatalf("exact cap result=%+v err=%v", result, err)
					}
					if diff := cmp.Diff([]machine.Signal{machine.ComputationError}, rec.signals); diff != "" {
						t.Fatalf("exact cap evidence (-want +got):\n%s", diff)
					}
				})
			})
		}
	}
}
