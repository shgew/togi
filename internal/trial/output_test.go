package trial

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"testing/synctest"
	"time"

	"github.com/google/go-cmp/cmp"
	"github.com/shgew/togi/internal/backend"
	"github.com/shgew/togi/internal/backend/mprime"
	"github.com/shgew/togi/internal/backend/ycruncher"
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
				_, err = buffer.consume(context.Background(), []byte(chunk), func(line string) { lines = append(lines, line) })
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
	if _, err := w.read(context.Background(), classify); err != nil {
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
	if _, err := w.read(context.Background(), classify); !errors.Is(err, errOutputLineTooLong) {
		t.Fatalf("large append error = %v", err)
	}
	if diff := cmp.Diff(prefix+"y", string(w.lines.pending)); diff != "" {
		t.Fatalf("retained prefix (-want +got):\n%s", diff)
	}
	if w.offset > outputLineLimit+4096 || len(lines) != 0 {
		t.Fatalf("read past oversized line: offset=%d lines=%q", w.offset, lines)
	}
	offset := w.offset
	if _, err := w.read(context.Background(), classify); err != nil || w.offset != offset || len(lines) != 0 {
		t.Fatalf("re-read rejected output: offset=%d lines=%q err=%v", w.offset, lines, err)
	}
}

func TestWatchedRejectsNonRegularFiles(t *testing.T) {
	for _, kind := range []string{"symlink", "directory", "fifo"} {
		t.Run(kind, func(t *testing.T) {
			dir := t.TempDir()
			path := filepath.Join(dir, "results.txt")
			if kind == "symlink" {
				target := filepath.Join(dir, "secret")
				if err := os.WriteFile(target, []byte("protected contents\n"), 0600); err != nil {
					t.Fatal(err)
				}
				if err := os.Symlink(target, path); err != nil {
					t.Fatal(err)
				}
			} else if kind == "fifo" {
				if err := syscall.Mkfifo(path, 0600); err != nil {
					t.Fatal(err)
				}
			} else if err := os.Mkdir(path, 0755); err != nil {
				t.Fatal(err)
			}
			w := watchFile{path: path}
			var lines []string
			_, err := w.read(context.Background(), func(line string) { lines = append(lines, line) })
			if err == nil || len(lines) != 0 || w.offset != 0 {
				t.Fatalf("unsafe watched file read: err=%v lines=%q offset=%d", err, lines, w.offset)
			}
		})
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
					data, err := os.ReadFile(filepath.Join(o.Dir, "oversized", "work", source+".log"))
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

func queueOutputConflict(t *testing.T, trial *running, line string, capFirst bool) {
	t.Helper()
	p := trial.instances[0].process.(*fakeProc)
	writes := []struct {
		out  io.Writer
		text string
	}{{p.stderrW, line + "\n"}, {p.stdoutW, strings.Repeat("x", outputLineLimit+1)}}
	if capFirst {
		writes[0], writes[1] = writes[1], writes[0]
	}
	for _, write := range writes {
		if _, err := io.WriteString(write.out, write.text); err != nil {
			t.Fatal(err)
		}
		synctest.Wait()
	}
}

func TestOutputLimitEvidencePrecedence(t *testing.T) {
	for _, evidence := range []struct {
		name, line string
		signal     machine.Signal
		escaped    []int
	}{
		{"affinity", "Failed to set core affinity to core: 42", "", []int{42}},
		{"computation", "Checksum mismatch", machine.ComputationError, nil},
	} {
		for _, order := range []string{"cap first", "evidence first"} {
			t.Run(evidence.name+"/"+order, func(t *testing.T) {
				synctest.Test(t, func(t *testing.T) {
					o := fakeOptions(t, "sleep")
					r := New(o)
					r.host = &fakeHost{}
					started, err := r.Start(context.Background(), testSpec("conflicting-output", machine.R1, time.Minute))
					if err != nil {
						t.Fatal(err)
					}
					trial := started.(*running)
					trial.backend = ycruncher.New("")
					queueOutputConflict(t, trial, evidence.line, order == "cap first")
					begin := time.Now()
					result, err := trial.Wait(context.Background(), &recorder{})
					if err != nil || result.Signal != evidence.signal || result.Inconclusive != "" {
						t.Fatalf("conflicting output result=%+v err=%v", result, err)
					}
					if diff := cmp.Diff(evidence.escaped, result.Escaped); diff != "" {
						t.Fatalf("escaped CPUs (-want +got):\n%s", diff)
					}
					if elapsed := time.Since(begin); elapsed > teardownLimit {
						t.Fatalf("conflicting output teardown exceeded deadline: %s", elapsed)
					}
					t.Logf("%s: escaped=%v signal=%s; no generic cap error; bounded teardown", order, result.Escaped, result.Signal)
				})
			})
		}
	}
}

func TestOutputLimitRetainsCleanupAndIOErrors(t *testing.T) {
	for _, failure := range []string{"scope cleanup", "output IO"} {
		t.Run(failure, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, "sleep")
				o.NoScope = false
				h := &fakeHost{inScope: true}
				if failure == "scope cleanup" {
					h.killScope = func(string) ([]byte, error) { return nil, io.ErrClosedPipe }
				}
				r := New(o)
				r.host = h
				started, err := r.Start(context.Background(), testSpec("output-cleanup", machine.R1, time.Minute))
				if err != nil {
					t.Fatal(err)
				}
				trial := started.(*running)
				trial.backend = ycruncher.New("")
				queueOutputConflict(t, trial, "Failed to set core affinity to core: 42", true)
				if failure == "output IO" {
					trial.events <- streamEvent{err: io.ErrClosedPipe}
				}
				result, err := trial.Wait(context.Background(), &recorder{})
				if !errors.Is(err, io.ErrClosedPipe) {
					t.Fatalf("lost %s error: result=%+v err=%v", failure, result, err)
				}
				if failure == "scope cleanup" && !errors.Is(err, machine.ErrContainment) {
					t.Fatalf("cleanup lost containment error: %v", err)
				}
				if diff := cmp.Diff([]int{42}, result.Escaped); diff != "" {
					t.Fatalf("escaped CPUs (-want +got):\n%s", diff)
				}
			})
		})
	}
}

func TestOutputLimitPrecedesQueuedExit(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		o := fakeOptions(t, "sleep")
		r := New(o)
		r.host = &fakeHost{}
		started, err := r.Start(context.Background(), testSpec("oversized-exit", machine.R1, time.Minute))
		if err != nil {
			t.Fatal(err)
		}
		trial := started.(*running)
		p := trial.instances[0].process.(*fakeProc)
		if _, err := io.WriteString(p.stdoutW, strings.Repeat("x", outputLineLimit+1)); err != nil {
			t.Fatal(err)
		}
		synctest.Wait()
		p.finish(nil)
		synctest.Wait()
		result, err := trial.Wait(context.Background(), &recorder{})
		if !errors.Is(err, errOutputLineTooLong) || result.Inconclusive == "" || result.Signal != "" {
			t.Fatalf("oversized queued exit result=%+v err=%v", result, err)
		}
	})
}

type watchClassifyingBackend struct {
	backend.Backend
	classify func(string) backend.Line
}

func (b watchClassifyingBackend) Classify(line string) backend.Line {
	return b.classify(line)
}

type reportedOutputSignal struct {
	Core   int
	Signal machine.Signal
	Detail string
}

type outputEvidenceRecorder struct {
	recorder
	diagnostics []reportedOutputSignal
}

func (r *outputEvidenceRecorder) Signal(core int, signal machine.Signal, detail string) {
	r.recorder.Signal(core, signal, detail)
	r.diagnostics = append(r.diagnostics, reportedOutputSignal{core, signal, detail})
}

func TestWatchedShortLineFloodCancellation(t *testing.T) {
	path := filepath.Join(t.TempDir(), "results.txt")
	text := "first\n" + strings.Repeat("ok\n", watchReadLimit)
	if err := os.WriteFile(path, []byte(text), 0644); err != nil {
		t.Fatal(err)
	}
	w := watchFile{path: path}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	var lines []string
	drained, err := w.read(ctx, func(line string) {
		lines = append(lines, line)
		cancel()
	})
	if !errors.Is(err, context.Canceled) || drained || w.offset != int64(len("first\n")) {
		t.Fatalf("flood cancellation: drained=%t offset=%d err=%v", drained, w.offset, err)
	}
	if diff := cmp.Diff([]string{"first"}, lines); diff != "" {
		t.Fatalf("classified after cancellation (-want +got):\n%s", diff)
	}
	lines = nil
	for !drained {
		drained, err = w.read(context.Background(), func(line string) { lines = append(lines, line) })
		if err != nil {
			t.Fatalf("resumed flood: drained=%t offset=%d err=%v", drained, w.offset, err)
		}
	}
	for _, line := range lines {
		if line != "ok" {
			t.Fatalf("replayed or corrupted line after cancellation: %q", line)
		}
	}
	if want := strings.Count(text, "\n") - 1; len(lines) != want {
		t.Fatalf("lost or duplicated output after cancellation: lines=%d, want %d", len(lines), want)
	}
}

func TestWatchedFloodKeepsSupervision(t *testing.T) {
	for _, escape := range []bool{false, true} {
		t.Run(fmt.Sprintf("escape=%t", escape), func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				supervising := true
				o := fakeOptions(t, "watched")
				o.NoScope = false
				h := &outputHost{fakeHost: &fakeHost{inScope: true, killScope: func(string) ([]byte, error) {
					supervising = false
					return nil, nil
				}}, text: strings.Repeat("ok\n", 100), watch: true}
				r := New(o)
				r.host = h
				spec := testSpec("watch-supervision", machine.R1, 500*time.Millisecond)
				started, err := r.Start(context.Background(), spec)
				if err != nil {
					t.Fatal(err)
				}
				trial := started.(*running)
				trial.backend = watchClassifyingBackend{Backend: trial.backend, classify: func(line string) backend.Line {
					if supervising {
						time.Sleep(o.SampleInterval)
					}
					return classifyHelper(line)
				}}
				threadSamples, cpuSamples := 0, 0
				trial.host = samplingHost{fakeHost: h.fakeHost,
					threads: func(pid int) ([]thread, error) {
						threadSamples++
						if escape && threadSamples == 2 {
							return []thread{{TID: pid, CPU: 9}}, nil
						}
						return nil, nil
					},
					usage: func(pid int) (usage, error) {
						cpuSamples++
						return h.Usage(pid)
					},
				}
				result, err := trial.Wait(context.Background(), &recorder{})
				if err != nil {
					t.Fatal(err)
				}
				wantEscaped := []int(nil)
				wantThreadSamples, wantCPUSamples := 8, 8
				if escape {
					wantEscaped = []int{9}
					wantThreadSamples, wantCPUSamples = 2, 1
				}
				if diff := cmp.Diff(wantEscaped, result.Escaped); diff != "" {
					t.Errorf("escaped CPUs (-want +got):\n%s", diff)
				}
				if diff := cmp.Diff([]int{wantThreadSamples, wantCPUSamples}, []int{threadSamples, cpuSamples}); diff != "" {
					t.Errorf("thread and CPU samples (-want +got):\n%s", diff)
				}
			})
		})
	}
}

func TestWatchedFloodSamplesEveryReadyInstance(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		supervising := true
		o := fakeOptions(t, "watched")
		o.NoScope = false
		h := &outputHost{fakeHost: &fakeHost{inScope: true, killScope: func(string) ([]byte, error) {
			supervising = false
			return nil, nil
		}}, text: strings.Repeat("ok\n", 100), watch: true}
		r := New(o)
		r.host = h
		spec := testSpec("watch-all-supervision", machine.R7, 500*time.Millisecond)
		spec.Cores, spec.CPUs = []int{0, 1}, []int{0, 1}
		started, err := r.Start(context.Background(), spec)
		if err != nil {
			t.Fatal(err)
		}
		trial := started.(*running)
		trial.backend = watchClassifyingBackend{Backend: trial.backend, classify: func(line string) backend.Line {
			if supervising {
				time.Sleep(o.SampleInterval)
			}
			return classifyHelper(line)
		}}
		ctx, cancel := context.WithCancel(context.Background())
		defer cancel()
		threadSamples, cpuSamples := map[int]int{}, map[int]int{}
		trial.host = samplingHost{fakeHost: h.fakeHost,
			threads: func(pid int) ([]thread, error) {
				threadSamples[pid]++
				return nil, nil
			},
			usage: func(pid int) (usage, error) {
				cpuSamples[pid]++
				if cpuSamples[trial.instances[0].PID] == 3 && cpuSamples[trial.instances[1].PID] == 3 {
					cancel()
				}
				return h.Usage(pid)
			},
		}
		result, err := trial.Wait(ctx, &recorder{})
		if !errors.Is(err, context.Canceled) {
			t.Fatalf("flood prevented timely cancellation: result=%+v err=%v", result, err)
		}
		want := map[int]int{trial.instances[0].PID: 3, trial.instances[1].PID: 3}
		if diff := cmp.Diff(want, threadSamples); diff != "" {
			t.Errorf("per-instance thread samples (-want +got):\n%s", diff)
		}
		if diff := cmp.Diff(want, cpuSamples); diff != "" {
			t.Errorf("per-instance CPU samples (-want +got):\n%s", diff)
		}
		if diff := cmp.Diff(200*time.Millisecond, result.Ran); diff != "" {
			t.Errorf("supervision duration (-want +got):\n%s", diff)
		}
	})
}

func TestWatchedPollFairness(t *testing.T) {
	for _, decision := range []string{"computation", "cancellation"} {
		for _, floodInstance := range []int{0, 1} {
			t.Run(fmt.Sprintf("%s/flood-instance-%d", decision, floodInstance), func(t *testing.T) {
				synctest.Test(t, func(t *testing.T) {
					o := fakeOptions(t, "watched")
					h := &outputHost{fakeHost: &fakeHost{}, text: strings.Repeat("ok\n", watchReadLimit), watch: true}
					r := New(o)
					r.host = h
					spec := testSpec("watch-fairness", machine.R7, time.Minute)
					spec.Cores, spec.CPUs = []int{0, 1}, []int{0, 1}
					started, err := r.Start(context.Background(), spec)
					if err != nil {
						t.Fatal(err)
					}
					trial := started.(*running)
					flood := &trial.instances[floodInstance].watch[0]
					quiet := trial.instances[1-floodInstance]
					text := ""
					if decision == "computation" {
						text = "COMPUTE ERROR\n"
					}
					if err := os.WriteFile(quiet.watch[0].path, []byte(text), 0644); err != nil {
						t.Fatal(err)
					}
					ctx, cancel := context.WithCancel(context.Background())
					defer cancel()
					supervising := true
					quietBeforeDrain := false
					trial.backend = watchClassifyingBackend{Backend: trial.backend, classify: func(line string) backend.Line {
						if line == "COMPUTE ERROR" {
							quietBeforeDrain = flood.offset < int64(len(h.text))
							supervising = false
						} else if supervising {
							time.Sleep(o.SampleInterval)
						}
						return classifyHelper(line)
					}}
					sampledQuiet := false
					trial.host = samplingHost{fakeHost: h.fakeHost, threads: func(pid int) ([]thread, error) {
						if pid == quiet.PID {
							sampledQuiet = true
							if decision == "cancellation" {
								quietBeforeDrain = flood.offset < int64(len(h.text))
								supervising = false
								cancel()
							}
						}
						return nil, nil
					}}
					var rec outputEvidenceRecorder
					result, err := trial.Wait(ctx, &rec)
					// The initial poll, one interval of flood classification, and
					// the next quiet poll must not wait for context timer publication.
					if !quietBeforeDrain || result.Ran > 3*o.SampleInterval || errors.Is(err, machine.ErrContainment) || result.Inconclusive != "" {
						t.Fatalf("watched backlog starved supervision: quiet-before-drain=%t result=%+v err=%v", quietBeforeDrain, result, err)
					}
					if decision == "cancellation" {
						if !sampledQuiet || !errors.Is(err, context.Canceled) || result.Signal != "" || len(rec.diagnostics) != 0 {
							t.Fatalf("quiet instance cancellation: sampled=%t result=%+v diagnostics=%v err=%v", sampledQuiet, result, rec.diagnostics, err)
						}
					} else {
						if err != nil || result.Signal != machine.ComputationError || result.Core != quiet.Core {
							t.Fatalf("quiet instance computation evidence: result=%+v err=%v", result, err)
						}
						want := []reportedOutputSignal{{quiet.Core, machine.ComputationError, "COMPUTE ERROR"}}
						if diff := cmp.Diff(want, rec.diagnostics); diff != "" {
							t.Fatalf("starved computation diagnostic (-want +got):\n%s", diff)
						}
					}
				})
			})
		}
	}
}

// unpublishedDeadlineContext models an elapsed deadline before its timer
// goroutine has published cancellation through Err or Done.
type unpublishedDeadlineContext struct {
	context.Context
	deadline time.Time
}

func (c unpublishedDeadlineContext) Deadline() (time.Time, bool) {
	return c.deadline, true
}

func TestWatchedDeadlineBeforeCancellationPublication(t *testing.T) {
	for _, name := range []string{"already expired", "expires during poll"} {
		t.Run(name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, "watched")
				h := &outputHost{fakeHost: &fakeHost{}, text: strings.Repeat("ok\n", 100), watch: true}
				r := New(o)
				r.host = h
				spec := testSpec("watch-unpublished-deadline", machine.R7, time.Minute)
				spec.Cores, spec.CPUs = []int{0, 1}, []int{0, 1}
				started, err := r.Start(context.Background(), spec)
				if err != nil {
					t.Fatal(err)
				}
				trial := started.(*running)
				quiet := trial.instances[1]
				if err := os.WriteFile(quiet.watch[0].path, []byte("COMPUTE ERROR"), 0644); err != nil {
					t.Fatal(err)
				}
				begin := time.Now()
				var budget time.Duration
				if name == "expires during poll" {
					budget = o.SampleInterval + o.SampleInterval/2
				}
				ctx := unpublishedDeadlineContext{Context: context.Background(), deadline: begin.Add(budget)}
				trial.backend = watchClassifyingBackend{Backend: trial.backend, classify: func(line string) backend.Line {
					if line == "ok" && time.Now().Before(ctx.deadline) {
						time.Sleep(time.Until(ctx.deadline))
					}
					return classifyHelper(line)
				}}
				// Sampling after the elapsed deadline would falsely report an
				// affinity escape and suppress the retained computation evidence.
				trial.host = samplingHost{fakeHost: h.fakeHost, threads: func(pid int) ([]thread, error) {
					return []thread{{TID: pid, CPU: 9}}, nil
				}}
				var rec outputEvidenceRecorder
				result, err := trial.Wait(ctx, &rec)
				if !errors.Is(err, context.DeadlineExceeded) || errors.Is(err, machine.ErrContainment) || result.Ran != budget || len(result.Escaped) != 0 || result.Inconclusive != "" || result.Signal != machine.ComputationError || result.Core != quiet.Core {
					t.Fatalf("unpublished deadline bypassed: budget=%s result=%+v err=%v", budget, result, err)
				}
				want := []reportedOutputSignal{{quiet.Core, machine.ComputationError, "COMPUTE ERROR"}}
				if diff := cmp.Diff(want, rec.diagnostics); diff != "" {
					t.Fatalf("final computation diagnostic after elapsed deadline (-want +got):\n%s", diff)
				}
			})
		})
	}
}

func TestWatchedPartialLineAcrossBatches(t *testing.T) {
	path := filepath.Join(t.TempDir(), "results.txt")
	prefix := strings.Repeat("x\n", watchReadLimit/2-2)
	text := prefix + "COMPUTE ERROR\n"
	if err := os.WriteFile(path, []byte(text), 0644); err != nil {
		t.Fatal(err)
	}
	w := watchFile{path: path}
	var errorsFound []string
	classify := func(line string) {
		if classifyHelper(line).Kind == backend.ComputationError {
			errorsFound = append(errorsFound, line)
		}
	}
	drained, err := w.read(context.Background(), classify)
	if err != nil || drained || len(w.lines.pending) == 0 || len(errorsFound) != 0 {
		t.Fatalf("first batch: drained=%t offset=%d pending=%q errors=%q err=%v", drained, w.offset, w.lines.pending, errorsFound, err)
	}
	for range 2 {
		drained, err = w.read(context.Background(), classify)
		if err != nil || !drained || w.offset != int64(len(text)) || len(w.lines.pending) != 0 {
			t.Fatalf("completed batch: drained=%t offset=%d pending=%q err=%v", drained, w.offset, w.lines.pending, err)
		}
	}
	if diff := cmp.Diff([]string{"COMPUTE ERROR"}, errorsFound); diff != "" {
		t.Fatalf("split computation evidence (-want +got):\n%s", diff)
	}
}

func TestWatchedErrorBeyondReadBudget(t *testing.T) {
	for _, tt := range []struct {
		name, ending string
	}{
		{"newline", "\n"},
		{"EOF", ""},
	} {
		t.Run(tt.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, "watched")
				o.SampleInterval = 0
				text := strings.Repeat("ok\n", 4*1024*1024/len("ok\n")) + "COMPUTE ERROR" + tt.ending
				h := &outputHost{fakeHost: &fakeHost{}, text: text, watch: true}
				r := New(o)
				r.host = h
				started, err := r.Start(context.Background(), testSpec("beyond-budget", machine.R1, 20*time.Millisecond))
				if err != nil {
					t.Fatal(err)
				}
				trial := started.(*running)
				var rec outputEvidenceRecorder
				result, err := trial.Wait(context.Background(), &rec)
				if err != nil || result.Signal != machine.ComputationError || result.Inconclusive != "" {
					t.Fatalf("late watched evidence: result=%+v err=%v", result, err)
				}
				if diff := cmp.Diff([]machine.Signal{machine.ComputationError}, rec.signals); diff != "" {
					t.Fatalf("final reported evidence (-want +got):\n%s", diff)
				}
				want := []reportedOutputSignal{{0, machine.ComputationError, "COMPUTE ERROR"}}
				if diff := cmp.Diff(want, rec.diagnostics); diff != "" {
					t.Fatalf("final computation diagnostic (-want +got):\n%s", diff)
				}
				w := &trial.instances[0].watch[0]
				if w.offset != int64(len(text)) || len(w.lines.pending) != 0 {
					t.Fatalf("incomplete final drain: offset=%d pending=%q", w.offset, w.lines.pending)
				}
			})
		})
	}
}

func TestWatchedTeardownMultiInstanceDeadline(t *testing.T) {
	for _, count := range []int{1, 8, 32} {
		t.Run(fmt.Sprint(count), func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, "watched")
				o.NoScope = false
				h := &outputHost{fakeHost: &fakeHost{inScope: true}, text: strings.Repeat("ok\n", 100) + "COMPUTE ERROR", watch: true}
				r := New(o)
				r.host = h
				spec := testSpec("watched-deadline", machine.R7, 20*time.Millisecond)
				spec.Cores, spec.CPUs = make([]int, count), make([]int, count)
				for i := range count {
					spec.Cores[i], spec.CPUs[i] = i, i
				}
				started, err := r.Start(context.Background(), spec)
				if err != nil {
					t.Fatal(err)
				}
				trial := started.(*running)
				for _, inst := range trial.instances {
					path := filepath.Join(filepath.Dir(inst.watch[0].path), "second.txt")
					if err := os.WriteFile(path, []byte(h.text), 0644); err != nil {
						t.Fatal(err)
					}
					inst.watch = append(inst.watch, watchFile{path: path})
				}
				trial.host = &deadlineScopeHost{fakeHost: h.fakeHost}
				trial.backend = watchClassifyingBackend{Backend: trial.backend, classify: func(line string) backend.Line {
					time.Sleep(time.Second)
					return classifyHelper(line)
				}}
				begin := time.Now()
				var rec recorder
				result, err := trial.Wait(context.Background(), &rec)
				if !errors.Is(err, machine.ErrContainment) || !errors.Is(err, context.DeadlineExceeded) || !errors.Is(err, errOutputDrainUnconfirmed) || result.Signal != "" || len(rec.signals) != 0 {
					t.Fatalf("unprocessed watched output: result=%+v signals=%v err=%v", result, rec.signals, err)
				}
				if elapsed := time.Since(begin) - spec.Duration; elapsed != teardownLimit {
					t.Fatalf("%d instances with two files: teardown=%s, want %s", count, elapsed, teardownLimit)
				}
			})
		})
	}
}

func TestWatchedLostBacklogCannotPass(t *testing.T) {
	for _, loss := range []string{"removed", "truncated", "symlink", "fifo"} {
		t.Run(loss, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, "watched")
				prefixLines := watchReadLimit/2 - 2
				h := &outputHost{fakeHost: &fakeHost{}, text: strings.Repeat("x\n", prefixLines) + "COMPUTE ERROR\n", watch: true}
				r := New(o)
				r.host = h
				spec := testSpec("lost-backlog", machine.R1, 20*time.Millisecond)
				started, err := r.Start(context.Background(), spec)
				if err != nil {
					t.Fatal(err)
				}
				trial := started.(*running)
				path := trial.instances[0].watch[0].path
				classified := 0
				trial.backend = watchClassifyingBackend{Backend: trial.backend, classify: func(line string) backend.Line {
					if line == "x" {
						classified++
						if classified == prefixLines {
							var err error
							switch loss {
							case "removed":
								err = os.Remove(path)
							case "truncated":
								err = os.Truncate(path, 0)
							case "symlink", "fifo":
								err = os.Remove(path)
								if err == nil {
									if loss == "symlink" {
										target := filepath.Join(filepath.Dir(path), "replacement.txt")
										err = os.WriteFile(target, []byte("COMPUTE ERROR\n"), 0600)
										if err == nil {
											err = os.Symlink(target, path)
										}
									} else {
										err = syscall.Mkfifo(path, 0600)
									}
								}
							}
							if err != nil {
								t.Fatal(err)
							}
						}
					}
					return classifyHelper(line)
				}}
				begin := time.Now()
				var rec recorder
				result, err := trial.Wait(context.Background(), &rec)
				if !errors.Is(err, errWatchedOutputLost) || !errors.Is(err, errOutputDrainUnconfirmed) || !errors.Is(err, machine.ErrContainment) || result.Signal != "" || result.Inconclusive == "" || len(rec.signals) != 0 {
					t.Fatalf("lost unread computation evidence: result=%+v signals=%v err=%v", result, rec.signals, err)
				}
				if elapsed := time.Since(begin) - spec.Duration; elapsed > teardownLimit {
					t.Fatalf("lost backlog exceeded shared cleanup deadline: %s", elapsed)
				}
			})
		})
	}
}

func TestWatchedRejectedOutputIsInconclusive(t *testing.T) {
	for _, kind := range []string{"symlink", "fifo"} {
		t.Run(kind, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, "watched")
				r := New(o)
				r.host = &outputHost{fakeHost: &fakeHost{}}
				started, err := r.Start(context.Background(), testSpec("rejected-output", machine.R1, 20*time.Millisecond))
				if err != nil {
					t.Fatal(err)
				}
				trial := started.(*running)
				path := trial.instances[0].watch[0].path
				if kind == "symlink" {
					target := filepath.Join(filepath.Dir(path), "protected.txt")
					if err := os.WriteFile(target, []byte("COMPUTE ERROR\n"), 0600); err != nil {
						t.Fatal(err)
					}
					if err := os.Symlink(target, path); err != nil {
						t.Fatal(err)
					}
				} else if err := syscall.Mkfifo(path, 0600); err != nil {
					t.Fatal(err)
				}
				var rec recorder
				result, err := trial.Wait(context.Background(), &rec)
				if err == nil || result.Inconclusive == "" || result.Signal != "" || len(rec.signals) != 0 || errors.Is(err, machine.ErrContainment) || errors.Is(err, errOutputDrainUnconfirmed) {
					t.Fatalf("rejected watched output: result=%+v signals=%v err=%v", result, rec.signals, err)
				}
				if kind == "symlink" && !errors.Is(err, syscall.ELOOP) {
					t.Fatalf("symlink rejection lost underlying error: %v", err)
				}
				if kind == "fifo" && !strings.Contains(err.Error(), "watched file is not regular") {
					t.Fatalf("nonregular rejection lost underlying error: %v", err)
				}
			})
		})
	}
}

func TestWatchedMissingFileBeforeCreation(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		o := fakeOptions(t, "watched")
		r := New(o)
		r.host = &outputHost{fakeHost: &fakeHost{}}
		spec := testSpec("no-watched-output", machine.R1, 20*time.Millisecond)
		started, err := r.Start(context.Background(), spec)
		if err != nil {
			t.Fatal(err)
		}
		result, err := started.Wait(context.Background(), &recorder{})
		if err != nil || result.Signal != "" || result.Inconclusive != "" || result.Ran != spec.Duration {
			t.Fatalf("never-created watched file: result=%+v err=%v", result, err)
		}
	})
}

func TestWatchedPartialSurvivesGraceUntilScopeKill(t *testing.T) {
	for _, tt := range []struct {
		name, ending string
		killErr      error
		signal       machine.Signal
	}{
		{"newline", "\n", nil, machine.ComputationError},
		{"EOF", "", nil, machine.ComputationError},
		{"unconfirmed writers", "", io.ErrClosedPipe, ""},
	} {
		t.Run(tt.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, "watched")
				o.NoScope = false
				h := &outputHost{fakeHost: &fakeHost{inScope: true}, text: "FATAL ", watch: true}
				r := New(o)
				r.host = h
				started, err := r.Start(context.Background(), testSpec("descendant-partial", machine.R1, 20*time.Millisecond))
				if err != nil {
					t.Fatal(err)
				}
				trial := started.(*running)
				trial.backend = mprime.New("")
				path := trial.instances[0].watch[0].path
				h.killScope = func(string) ([]byte, error) {
					f, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND, 0)
					if err != nil {
						t.Fatal(err)
					}
					_, err = io.WriteString(f, "ERROR"+tt.ending)
					closeErr := f.Close()
					if err != nil || closeErr != nil {
						t.Fatalf("append descendant output: write=%v close=%v", err, closeErr)
					}
					return nil, tt.killErr
				}
				var rec outputEvidenceRecorder
				result, err := trial.Wait(context.Background(), &rec)
				if result.Signal != tt.signal || result.Inconclusive != "" || errors.Is(err, machine.ErrContainment) != (tt.killErr != nil) || errors.Is(err, io.ErrClosedPipe) != (tt.killErr != nil) {
					t.Fatalf("descendant final line: result=%+v signals=%v err=%v", result, rec.signals, err)
				}
				if tt.killErr == nil && err != nil {
					t.Fatal(err)
				}
				var wantSignals []machine.Signal
				if tt.signal != "" {
					wantSignals = []machine.Signal{tt.signal}
				}
				if diff := cmp.Diff(wantSignals, rec.signals); diff != "" {
					t.Fatalf("descendant computation evidence (-want +got):\n%s", diff)
				}
				var wantDiagnostics []reportedOutputSignal
				if tt.signal != "" {
					wantDiagnostics = []reportedOutputSignal{{0, tt.signal, "FATAL ERROR"}}
				}
				if diff := cmp.Diff(wantDiagnostics, rec.diagnostics); diff != "" {
					t.Fatalf("descendant final diagnostic (-want +got):\n%s", diff)
				}
			})
		})
	}
}

func TestWatchedQueuedExitBeyondReadBudget(t *testing.T) {
	for _, tt := range []struct {
		name, line   string
		signal       machine.Signal
		escaped      []int
		inconclusive bool
	}{
		{"computation", "COMPUTE ERROR", machine.ComputationError, nil, false},
		{"setup", "PIN FAILED", "", nil, true},
		{"affinity", "AFFINITY:42", "", []int{42}, false},
	} {
		t.Run(tt.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				o := fakeOptions(t, "watched")
				h := &outputHost{fakeHost: &fakeHost{}, text: strings.Repeat("ok\n", watchReadLimit) + tt.line, watch: true}
				r := New(o)
				r.host = h
				started, err := r.Start(context.Background(), testSpec("queued-watch", machine.R1, time.Minute))
				if err != nil {
					t.Fatal(err)
				}
				h.procs[0].finish(nil)
				synctest.Wait()
				var rec outputEvidenceRecorder
				result, err := started.Wait(context.Background(), &rec)
				if err != nil || result.Signal != tt.signal || (result.Inconclusive != "") != tt.inconclusive {
					t.Fatalf("queued exit with late evidence: result=%+v err=%v", result, err)
				}
				if diff := cmp.Diff(tt.escaped, result.Escaped); diff != "" {
					t.Fatalf("late containment evidence (-want +got):\n%s", diff)
				}
				for _, signal := range rec.signals {
					if signal == machine.UnexpectedExit {
						t.Fatalf("exit classified before watched evidence: %v", rec.signals)
					}
				}
				var wantDiagnostics []reportedOutputSignal
				if tt.signal != "" {
					wantDiagnostics = []reportedOutputSignal{{0, tt.signal, tt.line}}
				}
				if diff := cmp.Diff(wantDiagnostics, rec.diagnostics); diff != "" {
					t.Fatalf("late final diagnostic (-want +got):\n%s", diff)
				}
			})
		})
	}
}

type watchScheduleHost struct {
	*outputHost
	started  time.Time
	duration time.Duration
	steps    []time.Duration
}

func (h *watchScheduleHost) SignalGroup(p process, sig syscall.Signal) error {
	at := time.Since(h.started)
	if at < h.duration && (sig == syscall.SIGSTOP || sig == syscall.SIGCONT) {
		h.steps = append(h.steps, at)
	}
	return h.fakeHost.SignalGroup(p, sig)
}

func TestWatchedFloodPreservesLoadStepSchedule(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		o := fakeOptions(t, "watched")
		o.SampleInterval = 25 * time.Millisecond
		h := &watchScheduleHost{outputHost: &outputHost{fakeHost: &fakeHost{}, text: strings.Repeat("ok\n", 200), watch: true}}
		r := New(o)
		r.host = h
		spec := testSpec("watch-schedule", machine.R4, 220*time.Millisecond)
		started, err := r.Start(context.Background(), spec)
		if err != nil {
			t.Fatal(err)
		}
		trial := started.(*running)
		trial.backend = watchClassifyingBackend{Backend: trial.backend, classify: func(line string) backend.Line {
			time.Sleep(time.Millisecond)
			return classifyHelper(line)
		}}
		h.started, h.duration = time.Now(), spec.Duration
		result, err := trial.Wait(context.Background(), &recorder{})
		if err != nil || result.Signal != "" || result.Inconclusive != "" || result.Ran != spec.Duration {
			t.Fatalf("watched load-step trial: result=%+v err=%v", result, err)
		}
		want := []time.Duration{50 * time.Millisecond, 100 * time.Millisecond, 150 * time.Millisecond, 200 * time.Millisecond}
		if diff := cmp.Diff(want, h.steps); diff != "" {
			t.Fatalf("watched output delayed load steps (-want +got):\n%s", diff)
		}
	})
}
