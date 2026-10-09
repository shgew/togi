package watch

import (
	"strconv"
	"strings"

	"github.com/charmbracelet/x/ansi"
)

// canvas is the frame as a grid of cells. Placing styled text parses it into cells, so a later placement replaces
// the cells under it, styles included, and each line is written once with only the style changes it needs.
type canvas struct {
	cells  [][]cell
	extent []int // cells written on each row, so a row ends where its last placement ended
	width  int
}

type cell struct {
	text  string // one grapheme; empty for the second cell of a wide one
	style sgr
}

// sgr is the colour state a cell is drawn in. Attributes are SGR codes 1-9, one bit each.
type sgr struct {
	fg, bg string
	attrs  uint16
}

func newCanvas(p layout) canvas {
	c := canvas{cells: make([][]cell, p.height), extent: make([]int, p.height), width: p.width}
	for y := range c.cells {
		c.cells[y] = make([]cell, p.width)
		for x := range c.cells[y] {
			c.cells[y][x].text = " "
		}
	}
	return c
}

func (c *canvas) put(r rectangle, x, y int, text string) {
	c.place(r, x, y, text, true)
}

func (c *canvas) putCells(r rectangle, x, y int, text string) {
	c.place(r, x, y, text, false)
}

func (c *canvas) place(r rectangle, x, y int, text string, words bool) {
	if x < 0 || y < 0 || x >= r.w || y >= r.h || r.y+y < 0 || r.y+y >= len(c.cells) {
		return
	}
	x += r.x
	if x < 0 || x >= c.width {
		return
	}
	width := min(r.w-(x-r.x), c.width-x)
	text = consoleText(text)
	if words {
		text = cutWords(text, width)
	} else {
		text = ansi.Truncate(text, width, "")
	}
	row := c.cells[r.y+y]
	var style sgr
	var state byte
	for len(text) > 0 {
		seq, w, n, next := ansi.DecodeSequence(text, state, nil)
		state = next
		text = text[n:]
		switch {
		case w > 0:
			if x+w > len(row) {
				return
			}
			row[x] = cell{seq, style}
			for i := 1; i < w; i++ {
				row[x+i] = cell{"", style}
			}
			x += w
			c.extent[r.y+y] = max(c.extent[r.y+y], x)
		case strings.HasPrefix(seq, "\x1b[") && strings.HasSuffix(seq, "m"):
			style = style.apply(seq[2 : len(seq)-1])
		}
	}
}

func (c *canvas) rows(r rectangle, lines []string) {
	lines = boundedRows(lines, r.h)
	for y, line := range lines {
		c.put(r, 0, y, line)
	}
}

// lines writes each row once, changing style only where the next visible cell needs another.
func (c *canvas) lines() []string {
	out := make([]string, len(c.cells))
	var b strings.Builder
	for y, row := range c.cells {
		b.Reset()
		var current sgr
		for _, cl := range row[:c.extent[y]] {
			if cl.text == "" {
				continue
			}
			if cl.style != current && (cl.text != " " || !cl.style.blank() || !current.blank()) {
				b.WriteString(transition(cl.style))
				current = cl.style
			}
			b.WriteString(cl.text)
		}
		if current != (sgr{}) {
			b.WriteString("\x1b[m")
		}
		out[y] = b.String()
	}
	return out
}

// blank is true when a space in this style looks the same as in any other style without a background or attributes.
func (s sgr) blank() bool { return s.bg == "" && s.attrs == 0 }

func transition(to sgr) string {
	if to == (sgr{}) {
		return "\x1b[m"
	}
	params := []string{"0"}
	for code := 1; code <= 9; code++ {
		if to.attrs&(1<<code) != 0 {
			params = append(params, strconv.Itoa(code))
		}
	}
	if to.fg != "" {
		params = append(params, to.fg)
	}
	if to.bg != "" {
		params = append(params, to.bg)
	}
	return "\x1b[" + strings.Join(params, ";") + "m"
}

func (s sgr) apply(params string) sgr {
	if params == "" {
		return sgr{}
	}
	codes := strings.Split(params, ";")
	for i := 0; i < len(codes); i++ {
		code, err := strconv.Atoi(codes[i])
		if err != nil {
			continue
		}
		switch {
		case code == 0:
			s = sgr{}
		case code >= 1 && code <= 9:
			s.attrs |= 1 << code
		case code == 22:
			s.attrs &^= 1<<1 | 1<<2
		case code >= 23 && code <= 29:
			s.attrs &^= 1 << (code - 20)
		case code >= 30 && code <= 37, code >= 90 && code <= 97:
			s.fg = codes[i]
		case code == 39:
			s.fg = ""
		case code >= 40 && code <= 47, code >= 100 && code <= 107:
			s.bg = codes[i]
		case code == 49:
			s.bg = ""
		case code == 38 || code == 48:
			n := 0
			if i+1 < len(codes) {
				switch codes[i+1] {
				case "5":
					n = 2
				case "2":
					n = 4
				}
			}
			if n == 0 || i+n >= len(codes) {
				i = len(codes)
				continue
			}
			value := strings.Join(codes[i:i+n+1], ";")
			if code == 38 {
				s.fg = value
			} else {
				s.bg = value
			}
			i += n
		}
	}
	return s
}
