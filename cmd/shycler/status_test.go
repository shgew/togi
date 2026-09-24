package main

import (
	"bytes"
	"crypto/sha256"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"

	"code.marleb.org/shgew/shycler/internal/journal"
)

func TestStatusAndCert(t *testing.T) {
	dir := t.TempDir()
	var stdout, stderr bytes.Buffer
	if code := cli([]string{"--state-dir", dir, "run", "--sim", "1"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("run: exit %d, stderr %s", code, stderr.String())
	}
	st, err := journal.ReadState(dir)
	if err != nil {
		t.Fatal(err)
	}
	raw, err := os.ReadFile(filepath.Join(dir, "events.jsonl"))
	if err != nil {
		t.Fatal(err)
	}

	stdout.Reset()
	if code := cli([]string{"--state-dir", dir, "status"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("status: exit %d, stderr %s", code, stderr.String())
	}
	status := stdout.String()
	if want := fmt.Sprintf("tier bronze [#%d]", st.TierSeq); !strings.Contains(status, want) {
		t.Fatalf("status lacks %q:\n%s", want, status)
	}
	checkRows(t, "status", status, regexp.MustCompile(`(?m)^(\d\d)  +\d  +(-?\d+)  `), st)

	stdout.Reset()
	if code := cli([]string{"--state-dir", dir, "cert"}, &stdout, &stderr); code != exitOK {
		t.Fatalf("cert: exit %d, stderr %s", code, stderr.String())
	}
	cert := stdout.String()
	for _, want := range []string{
		fmt.Sprintf("BRONZE                    [tier.change #%d]", st.TierSeq),
		"Platinum  locked until shycler observe exists",
		fmt.Sprintf("Journal SHA-256 %x through seq %d", sha256.Sum256(raw), st.LastSeq),
	} {
		if !strings.Contains(cert, want) {
			t.Fatalf("cert lacks %q:\n%s", want, cert)
		}
	}
	checkRows(t, "cert", cert, regexp.MustCompile(`(?m)^  (\d\d)  +(-?\d+)  `), st)
}

func checkRows(t *testing.T, name, out string, row *regexp.Regexp, st journal.State) {
	t.Helper()
	rows := row.FindAllStringSubmatch(out, -1)
	if len(rows) != len(st.Cores) || len(rows) != 16 {
		t.Fatalf("%s: %d core rows, want 16:\n%s", name, len(rows), out)
	}
	for i, r := range rows {
		core, _ := strconv.Atoi(r[1])
		offset, _ := strconv.Atoi(r[2])
		if c := st.Cores[i]; core != c.Core || offset != c.Offset {
			t.Errorf("%s row %d: core %d offset %d, want core %d offset %d", name, i, core, offset, c.Core, c.Offset)
		}
	}
}
