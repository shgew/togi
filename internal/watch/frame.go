package watch

import "time"

// Drawn is one frame of a view: exactly the screen's height less one lines, each at most its width less one cells, so
// the last row and column are never written and the screen never scrolls. Scroll is how many lines the view can
// scroll. A non-zero Until says the frame does not change with time before then.
type Drawn struct {
	Lines  []string
	Scroll int
	Until  time.Time
}

// Frame draws a screen of the dashboard.
type Frame func(sc Screen) Drawn
