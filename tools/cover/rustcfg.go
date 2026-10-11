package main

import (
	"bytes"
	"strings"
	"unicode/utf8"
)

// cfgValue is what a cfg predicate evaluates to on the host: true, false, or unknown when another build may decide it either way.
type cfgValue int8

const (
	cfgFalse cfgValue = iota
	cfgTrue
	cfgUnknown
)

func (v cfgValue) String() string {
	return [...]string{"false", "true", "unknown"}[v]
}

// hostCfg is the cfg options of the coverage build's target and the names it decides.
// A name outside known, such as a Cargo feature, a custom --cfg or test, evaluates to unknown.
type hostCfg struct {
	set   map[string]bool
	known map[string]bool
}

// wellKnownCfg holds the target and compiler names whose absence from `rustc --print cfg` means false for the host.
var wellKnownCfg = []string{
	"target_os", "target_family", "target_arch", "target_env", "target_vendor", "target_abi", "target_pointer_width",
	"target_endian", "target_feature", "target_has_atomic", "panic", "unix", "windows", "debug_assertions",
	"overflow_checks", "coverage", "miri", "doc", "doctest", "proc_macro", "relocation_model", "ub_checks",
	"fmt_debug", "target_thread_local",
}

// newHostCfg returns the cfg of options printed as `rustc --print cfg` prints them: `unix` or `target_os="linux"`.
func newHostCfg(options ...string) hostCfg {
	cfg := hostCfg{set: map[string]bool{}, known: map[string]bool{}}
	for _, name := range wellKnownCfg {
		cfg.known[name] = true
	}
	for _, option := range options {
		cfg.set[option] = true
		name, _, _ := strings.Cut(option, "=")
		cfg.known[name] = true
	}
	return cfg
}

// cfgOut returns the line ranges of the items in src under a #[cfg(...)] or #![cfg(...)] predicate that is false or unknown under cfg.
// An outer attribute's item ends at its first semicolon or comma outside brackets, before the bracket closing around it, or at the end of its first brace block.
// An inner attribute's item ends at the brace closing the block holding it, or at the end of the file.
// Attributes inside macro invocations and macro_rules bodies are tokens, not attributes, and are skipped.
func cfgOut(src []byte, cfg hostCfg) []lineRange {
	var out []lineRange
	lineOf := func(i int) int { return 1 + bytes.Count(src[:i], []byte{'\n'}) }
	var blocks []int
	for i := 0; i < len(src); i++ {
		if next, ok := skipLiteral(src, i); ok {
			i = next - 1
			continue
		}
		switch src[i] {
		case '{':
			blocks = append(blocks, i)
		case '}':
			if len(blocks) > 0 {
				blocks = blocks[:len(blocks)-1]
			}
		case '!':
			if group := macroGroup(src, i); group >= 0 {
				end := matching(src, group)
				if end < 0 {
					return out
				}
				i = end
			}
		case '#':
			open := skipSpace(src, i+1)
			inner := open < len(src) && src[open] == '!'
			if inner {
				open = skipSpace(src, open+1)
			}
			if open >= len(src) || src[open] != '[' {
				continue
			}
			end := matching(src, open)
			if end < 0 {
				return out
			}
			start := i
			i = end
			value, isCfg := attrCfg(string(src[open+1:end]), cfg)
			if !isCfg || value == cfgTrue {
				continue
			}
			last := len(src)
			switch {
			case !inner:
				last = itemEnd(src, end+1)
			case len(blocks) > 0:
				if close := matching(src, blocks[len(blocks)-1]); close >= 0 {
					last = close + 1
				}
			}
			for last > end+1 && isSpace(src[last-1]) {
				last--
			}
			out = append(out, lineRange{lineOf(start), lineOf(last - 1)})
		}
	}
	return out
}

// macroGroup returns the index of the bracket opening the token tree of the macro invocation or macro_rules definition whose `!` is at bang, or -1.
func macroGroup(src []byte, bang int) int {
	j := bang
	for j > 0 && isSpace(src[j-1]) {
		j--
	}
	name := j
	for name > 0 && isIdent(src[name-1]) {
		name--
	}
	if name == j || src[name] >= '0' && src[name] <= '9' {
		return -1
	}
	switch string(src[name:j]) {
	case "if", "while", "match", "return", "break", "in", "else", "let", "mut", "move", "as", "yield":
		return -1
	}
	k := skipTrivia(src, bang+1)
	if string(src[name:j]) == "macro_rules" {
		ident := k
		for k < len(src) && isIdent(src[k]) {
			k++
		}
		if k == ident {
			return -1
		}
		k = skipTrivia(src, k)
	}
	if k < len(src) && (src[k] == '(' || src[k] == '[' || src[k] == '{') {
		return k
	}
	return -1
}

func isSpace(b byte) bool {
	return b == ' ' || b == '\t' || b == '\n' || b == '\r'
}

func skipSpace(src []byte, i int) int {
	for i < len(src) && isSpace(src[i]) {
		i++
	}
	return i
}

// skipTrivia returns the index of the first byte at or after i that is neither whitespace nor in a comment.
func skipTrivia(src []byte, i int) int {
	for i < len(src) {
		if isSpace(src[i]) {
			i++
			continue
		}
		if i+1 < len(src) && src[i] == '/' && (src[i+1] == '/' || src[i+1] == '*') {
			i, _ = skipLiteral(src, i)
			continue
		}
		break
	}
	return i
}

// skipLiteral returns the index after the comment, string, byte string, C string or character literal starting at i.
// A lifetime is not a literal.
func skipLiteral(src []byte, i int) (int, bool) {
	rest := string(src[i:min(len(src), i+2)])
	switch {
	case rest == "//":
		end := bytes.IndexByte(src[i:], '\n')
		if end < 0 {
			return len(src), true
		}
		return i + end, true
	case rest == "/*":
		depth := 0
		for j := i; j+1 < len(src); j++ {
			switch string(src[j : j+2]) {
			case "/*":
				depth++
				j++
			case "*/":
				depth--
				j++
				if depth == 0 {
					return j + 1, true
				}
			}
		}
		return len(src), true
	case src[i] == '"':
		return endOfString(src, i+1), true
	case src[i] == '\'':
		return endOfChar(src, i)
	case i > 0 && isIdent(src[i-1]):
		return i, false
	}
	j := i
	if j < len(src) && (src[j] == 'b' || src[j] == 'c') {
		j++
		if j < len(src) && src[j] == '"' {
			return endOfString(src, j+1), true
		}
		if src[i] == 'b' && j < len(src) && src[j] == '\'' {
			return endOfChar(src, j)
		}
	}
	if j < len(src) && src[j] == 'r' {
		j++
		hashes := 0
		for j < len(src) && src[j] == '#' {
			hashes++
			j++
		}
		if j < len(src) && src[j] == '"' {
			return endOfRawString(src, j+1, hashes), true
		}
	}
	return i, false
}

// endOfChar returns the index after the character literal whose quote is at i, or false for a lifetime or label.
func endOfChar(src []byte, i int) (int, bool) {
	if i+1 >= len(src) {
		return i, false
	}
	if src[i+1] == '\\' {
		end := bytes.IndexByte(src[min(len(src), i+3):], '\'')
		if end < 0 {
			return len(src), true
		}
		return i + 3 + end + 1, true
	}
	_, size := utf8.DecodeRune(src[i+1:])
	if i+1+size < len(src) && src[i+1+size] == '\'' {
		return i + 2 + size, true
	}
	return i, false
}

func isIdent(b byte) bool {
	return b == '_' || b >= '0' && b <= '9' || b >= 'a' && b <= 'z' || b >= 'A' && b <= 'Z' || b >= utf8.RuneSelf
}

func endOfString(src []byte, i int) int {
	for ; i < len(src); i++ {
		switch src[i] {
		case '\\':
			i++
		case '"':
			return i + 1
		}
	}
	return len(src)
}

func endOfRawString(src []byte, i, hashes int) int {
	closing := []byte("\"" + strings.Repeat("#", hashes))
	end := bytes.Index(src[i:], closing)
	if end < 0 {
		return len(src)
	}
	return i + end + len(closing)
}

// matching returns the index of the bracket closing the one at open, or -1.
func matching(src []byte, open int) int {
	depth := 0
	for i := open; i < len(src); i++ {
		if next, ok := skipLiteral(src, i); ok {
			i = next - 1
			continue
		}
		switch src[i] {
		case '(', '[', '{':
			depth++
		case ')', ']', '}':
			depth--
			if depth == 0 {
				return i
			}
		}
	}
	return -1
}

// itemEnd returns the index after the item starting at i: after its first semicolon or comma outside brackets and generics,
// at the unmatched bracket closing around it, or after its first brace block.
// Angle brackets count as generics only until an `=`, after which `<` and `>` are operators.
func itemEnd(src []byte, i int) int {
	depth, angles := 0, 0
	for ; i < len(src); i++ {
		if next, ok := skipLiteral(src, i); ok {
			i = next - 1
			continue
		}
		switch src[i] {
		case '(', '[':
			depth++
		case '{':
			if depth == 0 {
				if end := matching(src, i); end >= 0 {
					return end + 1
				}
				return len(src)
			}
			depth++
		case ')', ']', '}':
			if depth == 0 {
				return i
			}
			depth--
		case '<':
			if depth == 0 && angles >= 0 {
				angles++
			}
		case '>':
			if depth == 0 && angles > 0 && src[i-1] != '-' {
				angles--
			}
		case '=':
			if depth == 0 {
				angles = -1
			}
		case ';':
			if depth == 0 {
				return i + 1
			}
		case ',':
			if depth == 0 && angles <= 0 {
				return i + 1
			}
		}
	}
	return len(src)
}

// attrCfg evaluates the attribute whose text between the brackets is body, and reports whether it is a cfg attribute.
// A cfg attribute that does not parse is unknown.
func attrCfg(body string, cfg hostCfg) (cfgValue, bool) {
	toks, ok := cfgTokens(body)
	if len(toks) == 0 || toks[0] != (cfgToken{kind: cfgIdent, text: "cfg"}) {
		return cfgTrue, false
	}
	if !ok || len(toks) < 2 || toks[1].kind != '(' {
		return cfgUnknown, true
	}
	p := cfgParser{toks: toks, pos: 2, cfg: cfg}
	v, ok := p.predicate()
	if !ok {
		return cfgUnknown, true
	}
	p.accept(',')
	if !p.accept(')') || p.pos != len(toks) {
		return cfgUnknown, true
	}
	return v, true
}

// evalCfg evaluates a cfg predicate: a name, name = "value", true, false, all(...), any(...) or not(...).
// A name the host decides is true when the host sets it; any other name, an unsupported operator or a malformed predicate is unknown.
// not of unknown is unknown; all is false if any argument is false, else unknown if any is; any is true if any argument is true, else unknown if any is.
func evalCfg(predicate string, cfg hostCfg) cfgValue {
	toks, ok := cfgTokens(predicate)
	if !ok {
		return cfgUnknown
	}
	p := cfgParser{toks: toks, cfg: cfg}
	v, ok := p.predicate()
	if !ok || p.pos != len(toks) {
		return cfgUnknown
	}
	return v
}

const (
	cfgIdent  = 'i'
	cfgString = 's'
)

// cfgToken is one token of a cfg predicate: an identifier or string, whose text is its name or decoded value, or the punctuation byte in kind.
type cfgToken struct {
	kind byte
	text string
}

// cfgTokens splits a cfg predicate into tokens, dropping whitespace and comments, and reports whether all of s tokenized.
func cfgTokens(s string) ([]cfgToken, bool) {
	src := []byte(s)
	var toks []cfgToken
	for i := skipTrivia(src, 0); i < len(src); i = skipTrivia(src, i) {
		c := src[i]
		switch {
		case c == '(' || c == ')' || c == ',' || c == '=':
			toks = append(toks, cfgToken{kind: c})
			i++
		case c == '"':
			value, end, ok := cookedString(s, i+1)
			if !ok {
				return toks, false
			}
			toks = append(toks, cfgToken{kind: cfgString, text: value})
			i = end
		case c == 'r' && i+1 < len(src) && (src[i+1] == '"' || src[i+1] == '#'):
			hashes := 0
			j := i + 1
			for j < len(src) && src[j] == '#' {
				hashes++
				j++
			}
			switch {
			case j < len(src) && src[j] == '"':
				closing := []byte("\"" + strings.Repeat("#", hashes))
				length := bytes.Index(src[j+1:], closing)
				if length < 0 {
					return toks, false
				}
				toks = append(toks, cfgToken{kind: cfgString, text: s[j+1 : j+1+length]})
				i = j + 1 + length + len(closing)
			case hashes == 1 && j < len(src) && isIdent(src[j]):
				start := j
				for j < len(src) && isIdent(src[j]) {
					j++
				}
				toks = append(toks, cfgToken{kind: cfgIdent, text: s[start:j]})
				i = j
			default:
				return toks, false
			}
		case isIdent(c) && (c < '0' || c > '9'):
			start := i
			for i < len(src) && isIdent(src[i]) {
				i++
			}
			toks = append(toks, cfgToken{kind: cfgIdent, text: s[start:i]})
		default:
			return toks, false
		}
	}
	return toks, true
}

// cookedString decodes the string literal whose content starts at i and returns its value and the index after its closing quote.
func cookedString(s string, i int) (string, int, bool) {
	var b strings.Builder
	for i < len(s) {
		c := s[i]
		switch {
		case c == '"':
			return b.String(), i + 1, true
		case c != '\\':
			b.WriteByte(c)
			i++
			continue
		case i+1 >= len(s):
			return "", 0, false
		}
		i++
		switch e := s[i]; e {
		case 'n':
			b.WriteByte('\n')
		case 'r':
			b.WriteByte('\r')
		case 't':
			b.WriteByte('\t')
		case '0':
			b.WriteByte(0)
		case '\\', '"', '\'':
			b.WriteByte(e)
		case '\n':
			for i+1 < len(s) && isSpace(s[i+1]) {
				i++
			}
		case 'x':
			if i+2 >= len(s) {
				return "", 0, false
			}
			hi, lo := hexDigit(s[i+1]), hexDigit(s[i+2])
			if hi < 0 || lo < 0 || hi > 7 {
				return "", 0, false
			}
			b.WriteByte(byte(hi<<4 | lo))
			i += 2
		case 'u':
			end := strings.IndexByte(s[i:], '}')
			if i+1 >= len(s) || s[i+1] != '{' || end < 0 {
				return "", 0, false
			}
			r := 0
			for _, d := range s[i+2 : i+end] {
				if d == '_' {
					continue
				}
				if d > 0x7f || hexDigit(byte(d)) < 0 {
					return "", 0, false
				}
				r = r<<4 | hexDigit(byte(d))
				if r > utf8.MaxRune {
					return "", 0, false
				}
			}
			if !utf8.ValidRune(rune(r)) {
				return "", 0, false
			}
			b.WriteRune(rune(r))
			i += end
		default:
			return "", 0, false
		}
		i++
	}
	return "", 0, false
}

func hexDigit(c byte) int {
	switch {
	case c >= '0' && c <= '9':
		return int(c - '0')
	case c >= 'a' && c <= 'f':
		return int(c-'a') + 10
	case c >= 'A' && c <= 'F':
		return int(c-'A') + 10
	}
	return -1
}

type cfgParser struct {
	toks []cfgToken
	pos  int
	cfg  hostCfg
}

func (p *cfgParser) accept(kind byte) bool {
	if p.pos < len(p.toks) && p.toks[p.pos].kind == kind {
		p.pos++
		return true
	}
	return false
}

// predicate parses one predicate at p.pos and reports false when the tokens there do not form one.
func (p *cfgParser) predicate() (cfgValue, bool) {
	if p.pos >= len(p.toks) || p.toks[p.pos].kind != cfgIdent {
		return cfgUnknown, false
	}
	name := p.toks[p.pos].text
	p.pos++
	switch {
	case p.accept('('):
		return p.call(name)
	case p.accept('='):
		if p.pos >= len(p.toks) || p.toks[p.pos].kind != cfgString {
			return cfgUnknown, false
		}
		value := p.toks[p.pos].text
		p.pos++
		return p.option(name, name+"=\""+value+"\""), true
	case name == "true":
		return cfgTrue, true
	case name == "false":
		return cfgFalse, true
	}
	return p.option(name, name), true
}

func (p *cfgParser) option(name, option string) cfgValue {
	switch {
	case !p.cfg.known[name]:
		return cfgUnknown
	case p.cfg.set[option]:
		return cfgTrue
	}
	return cfgFalse
}

// call parses the arguments of name(...) after its opening parenthesis, through its closing one.
func (p *cfgParser) call(name string) (cfgValue, bool) {
	if name != "all" && name != "any" && name != "not" {
		for depth := 1; depth > 0; p.pos++ {
			if p.pos >= len(p.toks) {
				return cfgUnknown, false
			}
			switch p.toks[p.pos].kind {
			case '(':
				depth++
			case ')':
				depth--
			}
		}
		return cfgUnknown, true
	}
	var args []cfgValue
	for !p.accept(')') {
		v, ok := p.predicate()
		if !ok {
			return cfgUnknown, false
		}
		args = append(args, v)
		if !p.accept(',') {
			if !p.accept(')') {
				return cfgUnknown, false
			}
			break
		}
	}
	switch name {
	case "not":
		if len(args) != 1 {
			return cfgUnknown, false
		}
		switch args[0] {
		case cfgTrue:
			return cfgFalse, true
		case cfgFalse:
			return cfgTrue, true
		case cfgUnknown:
		}
		return cfgUnknown, true
	case "all":
		return combine(args, cfgFalse, cfgTrue), true
	default:
		return combine(args, cfgTrue, cfgFalse), true
	}
}

// combine returns decisive if any argument is decisive, else unknown if any argument is unknown, else otherwise.
func combine(args []cfgValue, decisive, otherwise cfgValue) cfgValue {
	result := otherwise
	for _, v := range args {
		if v == decisive {
			return decisive
		}
		if v == cfgUnknown {
			result = cfgUnknown
		}
	}
	return result
}
