package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

const (
	recordCommentURL = "https://github.com/shgew/togi/pull/700#issuecomment-5001"
	checkRunURL      = "https://github.com/shgew/togi/runs/77"
)

// fakeGitHub answers publish's gh calls and keeps what was posted.
type fakeGitHub struct {
	heads        []string // pr view answers in turn
	comments     string   // answer to the comment listing
	checkApp     string
	posted       []string // "comment: <first line>" and "check: <payload>"
	commentBody  string
	checkPayload string
	views        int
}

func (f *fakeGitHub) handle(args []string) ([]byte, error) {
	switch {
	case args[0] == "pr" && args[1] == "view":
		head := f.heads[min(f.views, len(f.heads)-1)]
		f.views++
		return fmt.Appendf(nil, `{"headRefOid":%q,"baseRefOid":%q,"url":%q}`, head, baseSHA, prURL), nil
	case args[0] == "api" && slices.Contains(args, "--paginate"):
		if f.comments == "" {
			return []byte("[[]]"), nil
		}
		return []byte(f.comments), nil
	case args[0] == "api" && strings.HasSuffix(args[3], "/issues/700/comments"):
		body, ok := strings.CutPrefix(args[5], "body=@")
		if !ok {
			return nil, fmt.Errorf("unexpected comment args %v", args)
		}
		b, err := os.ReadFile(body)
		if err != nil {
			return nil, err
		}
		f.commentBody = string(b)
		f.posted = append(f.posted, "comment")
		return fmt.Appendf(nil, `{"html_url":%q}`, recordCommentURL), nil
	case args[0] == "api" && strings.HasSuffix(args[3], "/check-runs"):
		b, err := os.ReadFile(args[5])
		if err != nil {
			return nil, err
		}
		f.checkPayload = string(b)
		f.posted = append(f.posted, "check")
		var p checkPayload
		if err := json.Unmarshal(b, &p); err != nil {
			return nil, err
		}
		slug := f.checkApp
		if slug == "" {
			slug = "robotogi"
		}
		return json.Marshal(map[string]any{"html_url": checkRunURL, "head_sha": p.HeadSHA, "app": map[string]string{"slug": slug}, "output": p.Output})
	}
	return nil, fmt.Errorf("unexpected gh %v", args)
}

// renderedSnapshot writes a manifest and its rendered record for a docs-only pull request, with the given finding outcome.
func renderedSnapshot(t *testing.T, o outcome) string {
	t.Helper()
	dir := t.TempDir()
	m := firstManifest(included("docs/howto.md", 5, 2))
	if err := writeManifest(dir, m); err != nil {
		t.Fatal(err)
	}
	in := findingsInput{
		Reviewers: []recordReviewer{{Name: "reviewer-1", Files: []string{"docs/howto.md"}}},
		Findings:  []recordFinding{{Source: "reviewer-1", Priority: "P1", Finding: "x", Outcome: o}},
	}
	b, _ := json.Marshal(in)
	file := filepath.Join(dir, "findings.json")
	if err := os.WriteFile(file, b, 0o644); err != nil {
		t.Fatal(err)
	}
	tl, _, _, _, _ := testTools(t, nil, nil)
	if err := runRender(tl, []string{dir, file}); err != nil {
		t.Fatal(err)
	}
	return dir
}

func TestPublishPostsRecordAndCheckOnTheReviewedHead(t *testing.T) {
	t.Parallel()
	dir := renderedSnapshot(t, outcome{Status: "fixed", SHA: fixSHA})
	f := &fakeGitHub{heads: []string{headSHA}}
	tl, g, _, out, _ := testTools(t, f.handle, nil)
	if err := runPublish(tl, []string{dir}); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]string{"comment", "check"}, f.posted); diff != "" {
		t.Errorf("posts (-want +got):\n%s", diff)
	}
	if got, want := f.commentBody, readFile(t, filepath.Join(dir, recordFile)); got != want {
		t.Error("the comment is not the rendered record")
	}
	wantCheck := `{"name":"review","head_sha":"` + headSHA + `","status":"completed","conclusion":"success","output":{"title":"Review recorded","summary":"Review record: ` + recordCommentURL + `"}}`
	if f.checkPayload != wantCheck {
		t.Errorf("check payload = %s, want %s", f.checkPayload, wantCheck)
	}
	if want := "record " + recordCommentURL + "\ncheck " + checkRunURL + "\n"; out.String() != want {
		t.Errorf("output = %q, want %q", out.String(), want)
	}
	// The head is checked before posting and again after.
	var views int
	for _, c := range g.calls {
		if c.Args[0] == "pr" {
			views++
		}
	}
	if views != 2 {
		t.Errorf("checked the head %d times, want 2", views)
	}
}

func TestPublishRefusesMovedHead(t *testing.T) {
	t.Parallel()
	dir := renderedSnapshot(t, outcome{Status: "fixed", SHA: fixSHA})
	f := &fakeGitHub{heads: []string{movedSHA}}
	tl, _, _, out, _ := testTools(t, f.handle, nil)
	err := runPublish(tl, []string{dir})
	if err == nil || !strings.Contains(err.Error(), "not the snapshot's "+headSHA) {
		t.Fatalf("error = %v, want a refusal naming the snapshot's head", err)
	}
	if len(f.posted) != 0 || out.Len() != 0 {
		t.Errorf("a refused publish posted %v and printed %q", f.posted, out.String())
	}
}

func TestPublishWarnsWhenHeadMovesAfterPosting(t *testing.T) {
	t.Parallel()
	dir := renderedSnapshot(t, outcome{Status: "fixed", SHA: fixSHA})
	f := &fakeGitHub{heads: []string{headSHA, movedSHA}}
	tl, _, _, out, errOut := testTools(t, f.handle, nil)
	if err := runPublish(tl, []string{dir}); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out.String(), recordCommentURL) || !strings.Contains(errOut.String(), "needs its own review") {
		t.Errorf("stdout %q, stderr %q", out.String(), errOut.String())
	}
}

func TestPublishNeedsOneToken(t *testing.T) {
	t.Parallel()
	dir := renderedSnapshot(t, outcome{Status: "fixed", SHA: fixSHA})
	f := &fakeGitHub{heads: []string{headSHA}}
	tl, _, _, _, _ := testTools(t, f.handle, nil)
	tl.getenv = func(string) string { return "" }
	if err := runPublish(tl, []string{dir}); err == nil || !strings.Contains(err.Error(), "just as-bot") {
		t.Fatalf("error = %v, want the as-bot hint", err)
	}
	if f.views != 0 {
		t.Error("publish called gh without a token")
	}
}

func TestPublishBlockedPostsNoCheck(t *testing.T) {
	t.Parallel()
	dir := renderedSnapshot(t, outcome{Status: "deferred", Issue: 5})
	f := &fakeGitHub{heads: []string{headSHA}}
	tl, _, _, out, _ := testTools(t, f.handle, nil)
	if err := runPublish(tl, []string{dir}); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]string{"comment"}, f.posted); diff != "" {
		t.Errorf("posts (-want +got):\n%s", diff)
	}
	if !strings.Contains(out.String(), "blocked: no review check posted") {
		t.Errorf("output = %q", out.String())
	}
}

func TestPublishReusesTheRecordOnTheHead(t *testing.T) {
	t.Parallel()
	dir := renderedSnapshot(t, outcome{Status: "fixed", SHA: fixSHA})
	existing := readFile(t, filepath.Join(dir, recordFile))
	comments, _ := json.Marshal([][]map[string]any{{
		{"html_url": "https://example.test/other", "body": "hello", "user": map[string]string{"login": "shgew"}},
		{"html_url": recordCommentURL, "body": existing, "user": map[string]string{"login": "robotogi[bot]"}},
	}})
	f := &fakeGitHub{heads: []string{headSHA}, comments: string(comments)}
	tl, _, _, out, _ := testTools(t, f.handle, nil)
	if err := runPublish(tl, []string{dir}); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]string{"check"}, f.posted); diff != "" {
		t.Errorf("posts (-want +got):\n%s", diff)
	}
	if !strings.Contains(out.String(), "record "+recordCommentURL) {
		t.Errorf("output = %q", out.String())
	}
}

func TestPublishIgnoresRecordsFromOthers(t *testing.T) {
	t.Parallel()
	dir := renderedSnapshot(t, outcome{Status: "fixed", SHA: fixSHA})
	existing := readFile(t, filepath.Join(dir, recordFile))
	comments, _ := json.Marshal([][]map[string]any{{{"html_url": "https://example.test/copy", "body": existing, "user": map[string]string{"login": "shgew"}}}})
	f := &fakeGitHub{heads: []string{headSHA}, comments: string(comments)}
	tl, _, _, _, _ := testTools(t, f.handle, nil)
	if err := runPublish(tl, []string{dir}); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]string{"comment", "check"}, f.posted); diff != "" {
		t.Errorf("posts (-want +got):\n%s", diff)
	}
}

func TestPublishRejectsForeignCheck(t *testing.T) {
	t.Parallel()
	dir := renderedSnapshot(t, outcome{Status: "fixed", SHA: fixSHA})
	f := &fakeGitHub{heads: []string{headSHA}, checkApp: "github-actions"}
	tl, _, _, _, _ := testTools(t, f.handle, nil)
	if err := runPublish(tl, []string{dir}); err == nil || !strings.Contains(err.Error(), `belongs to app "github-actions"`) {
		t.Fatalf("error = %v, want a foreign-app refusal", err)
	}
}

func TestPublishNeedsARenderedRecordForTheHead(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	if err := writeManifest(dir, firstManifest(included("docs/x.md", 1, 0))); err != nil {
		t.Fatal(err)
	}
	f := &fakeGitHub{heads: []string{headSHA}}
	tl, _, _, _, _ := testTools(t, f.handle, nil)
	if err := runPublish(tl, []string{dir}); err == nil || !strings.Contains(err.Error(), "run render first") {
		t.Fatalf("error = %v, want a render hint", err)
	}
	stale := renderedSnapshot(t, outcome{Status: "fixed", SHA: fixSHA})
	m, _ := readManifest(stale)
	m.HeadSHA = movedSHA
	if err := writeManifest(stale, m); err != nil {
		t.Fatal(err)
	}
	f = &fakeGitHub{heads: []string{movedSHA}}
	tl, _, _, _, _ = testTools(t, f.handle, nil)
	if err := runPublish(tl, []string{stale}); err == nil || !strings.Contains(err.Error(), "run render again") {
		t.Fatalf("error = %v, want a stale-record refusal", err)
	}
}

func TestPublishCarriesForward(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	m := firstManifest()
	m.CarryForward = true
	m.Previous = &previousRecord{URL: previousURL, HeadSHA: oldHeadSHA, BaseSHA: oldBaseSHA, Verdict: "success"}
	m.Patches = []patchStatus{{Status: "unchanged"}, {Status: "unchanged", ContextOnly: true}}
	if err := writeManifest(dir, m); err != nil {
		t.Fatal(err)
	}
	f := &fakeGitHub{heads: []string{headSHA}}
	tl, _, _, out, _ := testTools(t, f.handle, nil)
	if err := runPublish(tl, []string{dir}); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]string{"check"}, f.posted); diff != "" {
		t.Errorf("posts (-want +got):\n%s", diff)
	}
	var p checkPayload
	if err := json.Unmarshal([]byte(f.checkPayload), &p); err != nil {
		t.Fatal(err)
	}
	want := "Review record: " + previousURL + " carried forward. git range-diff " + oldBaseSHA + ".." + oldHeadSHA + " " + baseSHA + ".." + headSHA + " pairs all 2 patches unchanged, 1 of them with only context lines changed."
	if p.HeadSHA != headSHA || p.Output.Summary != want {
		t.Errorf("payload = %+v, want summary %q", p, want)
	}
	if !strings.Contains(out.String(), "record "+previousURL) {
		t.Errorf("output = %q", out.String())
	}
}

func TestPublishRefusesCarryForwardOfAPriorThatIsNotASuccess(t *testing.T) {
	t.Parallel()
	for _, verdict := range []string{"blocked", ""} {
		dir := t.TempDir()
		m := firstManifest()
		m.CarryForward = true
		m.Previous = &previousRecord{URL: previousURL, HeadSHA: oldHeadSHA, BaseSHA: oldBaseSHA, Verdict: verdict}
		m.Patches = []patchStatus{{Status: "unchanged"}}
		if err := writeManifest(dir, m); err != nil {
			t.Fatal(err)
		}
		f := &fakeGitHub{heads: []string{headSHA}}
		tl, _, _, out, _ := testTools(t, f.handle, nil)
		if err := runPublish(tl, []string{dir}); err == nil || !strings.Contains(err.Error(), "not recorded as a success") {
			t.Fatalf("verdict %q: error = %v, want a carry-forward refusal", verdict, err)
		}
		if len(f.posted) != 0 || out.Len() != 0 {
			t.Errorf("verdict %q: a refused carry-forward posted %v and printed %q", verdict, f.posted, out.String())
		}
	}
}

func TestPublishIgnoresPrefixLookalikeBots(t *testing.T) {
	t.Parallel()
	dir := renderedSnapshot(t, outcome{Status: "fixed", SHA: fixSHA})
	existing := readFile(t, filepath.Join(dir, recordFile))
	var users []map[string]any
	for i, login := range []string{"robotogi-helper", "robotogi", "robotogi[bot]x"} {
		users = append(users, map[string]any{"html_url": fmt.Sprintf("https://example.test/lookalike-%d", i), "body": existing, "user": map[string]string{"login": login}})
	}
	comments, _ := json.Marshal([][]map[string]any{users})
	f := &fakeGitHub{heads: []string{headSHA}, comments: string(comments)}
	tl, _, _, out, _ := testTools(t, f.handle, nil)
	if err := runPublish(tl, []string{dir}); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]string{"comment", "check"}, f.posted); diff != "" {
		t.Errorf("posts (-want +got):\n%s", diff)
	}
	if strings.Contains(out.String(), "example.test") || !strings.Contains(f.checkPayload, recordCommentURL) || strings.Contains(f.checkPayload, "example.test") {
		t.Errorf("a lookalike's comment was linked: output %q, check %s", out.String(), f.checkPayload)
	}
}

func TestPublishRefusesAnExistingRecordThatDiffers(t *testing.T) {
	t.Parallel()
	blockedDir := renderedSnapshot(t, outcome{Status: "deferred", Issue: 5})
	blocked := readFile(t, filepath.Join(blockedDir, recordFile))
	// The same head rendered again with the finding fixed: the local record is a success, the posted one is blocked.
	dir := renderedSnapshot(t, outcome{Status: "fixed", SHA: fixSHA})
	for name, body := range map[string]string{
		"blocked record": blocked,
		"other data":     readFile(t, filepath.Join(dir, recordFile)) + "\nextra visible text\n",
	} {
		comments, _ := json.Marshal([][]map[string]any{{{"html_url": recordCommentURL, "body": body, "user": map[string]string{"login": "robotogi[bot]"}}}})
		f := &fakeGitHub{heads: []string{headSHA}, comments: string(comments)}
		tl, _, _, out, _ := testTools(t, f.handle, nil)
		err := runPublish(tl, []string{dir})
		if err == nil || !strings.Contains(err.Error(), "differs from "+recordFile) {
			t.Fatalf("%s: error = %v, want a mismatch refusal", name, err)
		}
		if len(f.posted) != 0 || f.checkPayload != "" || out.Len() != 0 {
			t.Errorf("%s: a refused publish posted %v, check %q, output %q", name, f.posted, f.checkPayload, out.String())
		}
	}
}

func TestPublishReusesAnIdenticalRecordDespiteLineEndings(t *testing.T) {
	t.Parallel()
	dir := renderedSnapshot(t, outcome{Status: "fixed", SHA: fixSHA})
	body := strings.ReplaceAll(readFile(t, filepath.Join(dir, recordFile)), "\n", "\r\n")
	comments, _ := json.Marshal([][]map[string]any{{{"html_url": recordCommentURL, "body": body, "user": map[string]string{"login": "robotogi[bot]"}}}})
	f := &fakeGitHub{heads: []string{headSHA}, comments: string(comments)}
	tl, _, _, _, _ := testTools(t, f.handle, nil)
	if err := runPublish(tl, []string{dir}); err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff([]string{"check"}, f.posted); diff != "" {
		t.Errorf("posts (-want +got):\n%s", diff)
	}
}

func TestPublishUsage(t *testing.T) {
	t.Parallel()
	tl, _, _, _, _ := testTools(t, nil, nil)
	if err := runPublish(tl, nil); err == nil || !strings.Contains(err.Error(), "usage:") {
		t.Fatalf("error = %v, want usage", err)
	}
}

func TestDispatch(t *testing.T) {
	t.Parallel()
	tl, _, _, _, _ := testTools(t, func([]string) ([]byte, error) {
		return []byte(`[{"data":{"repository":{"pullRequests":{"nodes":[]}}}}]`), nil
	}, nil)
	for _, args := range [][]string{nil, {"report"}} {
		if err := dispatch(tl, args); err != nil {
			t.Errorf("dispatch(%v): %v", args, err)
		}
	}
	for _, args := range [][]string{{"report", "x"}, {"bogus"}} {
		if err := dispatch(tl, args); err == nil {
			t.Errorf("dispatch(%v) succeeded", args)
		}
	}
}
