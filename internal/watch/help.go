package watch

import (
	"fmt"

	"github.com/shgew/togi/internal/machine"
)

type helpSection struct {
	title string
	items []helpItem
}

type helpItem struct {
	label, text, example string
}

var help = []helpSection{
	{"What I do", []helpItem{
		{"Find limits", "One core at a time, with every other core at 0, I move its offset deeper until a stress test fails, then confirm the deepest offset that passed with several passes in a row. That is its solo limit.", ""},
		{"Test together", "All cores run at their offsets at once, through a lap: the configured list of tests. A full lap covers every required kind of load. Cores can be fine alone and still fail together.", ""},
		{"Find the culprit", "When a test fails and nothing names a single core, I pause the lap and hunt: I rerun the test with some cores parked and narrow down, by halves, which cores cause it. I may find one core or a combination that fails together. If all tested groups pass, the suspects remain unresolved, and I keep their offsets shallower together as a precaution. A hunt can come at any time, so nothing can say how long tuning takes.", ""},
		{"Go deeper", "Backing off after a failure can leave room elsewhere. After a clean lap I try to win depth back, checking every move.", ""},
		{"Clean lap", "The goal: a full lap with all its checks passed at the resulting offsets and no room left to deepen. Earlier failures can lead to backing off and finishing this same lap.", ""},
		{"Keep checking", "After that I keep running laps to catch rarer failures until you stop me.", ""},
	}},
	{"Reading the screen", []helpItem{
		{"Bright rows", "The cores this test is judging. Grey rows are not being judged right now.", ""},
		{"►", "The cores the load runs on. During a hunt they can differ from the bright ones: parked cores carry the load at 0 while the suspects sit at the offsets that failed.", ""},
		{"The number", "The offset applied right now. After a stop or dead end I show what was found instead, except at an SMU dead end, when I keep the last hardware readback. 0 is no undervolt, -50 is the deepest Curve Optimizer allows.", ""},
		{"The bar", "One cell per count, from 0 on the left to -50 on the right, filled up to the displayed offset. A faint bar shows the offset a parked core goes back to.", ""},
		{"The red tick", "The failed offset attributed to this core, including failures with other cores at nonzero offsets. Tuning stays shallower than this failure point.", ""},
		{"HUNT", "Lights up while a hunt pauses the stages.", ""},
		{"What happened", "Recent events, newest first, each with a tag for its kind: pass, fail, crash, hunt and so on. A test that passed several times in a row takes one line, with the hottest CPU temperature (Tctl) any run reached. L shows the journal itself.", ""},
	}},
	{"Words", []helpItem{
		{"searching", "Still finding its solo limit.", ""},
		{"confirming", "Repeating its deepest pass to be sure of it.", ""},
		{"limit found", "One count deeper failed. It stays here, and every lap still tests it.", ""},
		{"maxed out", "At -50, the deepest Curve Optimizer allows.", ""},
		{"can go deeper", "Nothing known stops it going deeper. The next deepening round will try.", ""},
		{"held back by others", "Combination restrictions keep it shallower than its own failure point, so it cannot go deeper at the other cores' current offsets.", ""},
		{"suspect", "During a hunt: back at the offset it failed with, so this test tells whether it is part of the cause.", ""},
		{"parked", "During a hunt: held at an offset that passed before, usually 0, so it can't be the cause.", ""},
		{"solo limit", "The deepest offset a core passed with every other core at 0.", ""},
		{"combination", "Offsets that must not be reached together. A hunt may prove the group fails together, or leave an unresolved group restricted as a precaution.", ""},
		{"lap", "One pass through the configured list of tests. A clean lap has passing evidence for each scheduled check at the resulting offsets. Only a full lap covers every required kind of load.", ""},
		{"recorded only", "A partial all-core part of a lap step, with the cores at their CCD's shallowest offset left idle. Its result is kept on record but moves no offset. The lap needs it to finish, pass or fail, and it covers no required kind of load.", ""},
		{"run", "One launch of one test. Confirming a solo limit, a hunt group or a deepening move needs several passes in a row. Ordinary lap steps can advance after one pass, while all-core steps also include repeated short runs and a long run.", ""},
	}},
}

// regimeLike names everyday work each kind of test resembles.
var regimeLike = map[machine.Regime]string{
	machine.R1: "Like a game's main thread, opening apps or loading web pages.",
	machine.R2: "Like video encoding, 3D rendering or scientific math on one thread.",
	machine.R3: "Like a game loading a level, or apps waking up for short tasks.",
	machine.R4: "Like background work: a video call, music playback, a download.",
	machine.R5: "Like compiling or other work that keeps both threads of a core busy.",
	machine.R6: "Like a desktop left alone, or reading and typing.",
	machine.R7: "Like rendering, compiling or exporting video on every core.",
}

func helpLines(width int) []string {
	var out []string
	for i, sec := range append(help, testsSection()) {
		if i > 0 {
			out = append(out, "", "")
		}
		out = append(out, amber.Render(sec.title))
		labelWidth := 0
		for _, it := range sec.items {
			labelWidth = max(labelWidth, len([]rune(it.label)))
		}
		labelWidth += 3
		tw := width - 2 - labelWidth
		for j, it := range sec.items {
			if j > 0 {
				out = append(out, "")
			}
			lines := wrapStyled(it.text, tw, textStyle)
			if it.example != "" {
				lines = append(lines, wrapStyled(it.example, tw, grey)...)
			}
			for k, l := range lines {
				label := ""
				if k == 0 {
					label = it.label
				}
				out = append(out, "  "+white.Render(fmt.Sprintf("%-*s", labelWidth, label))+l)
			}
		}
	}
	return append(out, "", "", grey.Render("Passing tests can't prove offsets will never fail. They show which tests passed, and more laps catch rarer failures."))
}

func testsSection() helpSection {
	sec := helpSection{title: "The kinds of test"}
	for _, r := range []machine.Regime{machine.R1, machine.R2, machine.R3, machine.R4, machine.R5, machine.R6, machine.R7} {
		sec.items = append(sec.items, helpItem{regimeWords[r], capital(regimeExplained[r]) + ".", regimeLike[r]})
	}
	return sec
}
