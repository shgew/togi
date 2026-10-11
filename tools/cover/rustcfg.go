package main

import (
	"bytes"
	"strings"
)

// cfgOut returns the line ranges of the items in src whose #[cfg(...)] or #![cfg(...)] predicate is false under cfg.
// An item runs from its attribute to the end of its braces or to its first top-level semicolon.
func cfgOut(src []byte, cfg map[string]bool) []lineRange {
	var out []lineRange
	lineOf := func(i int) int { return 1 + bytes.Count(src[:i], []byte{'\n'}) }
	for i := 0; i < len(src); i++ {
		if next, ok := skipLiteral(src, i); ok {
			i = next - 1
			continue
		}
		if src[i] != '#' {
			continue
		}
		open := i + 1
		inner := open < len(src) && src[open] == '!'
		if inner {
			open++
		}
		if open >= len(src) || src[open] != '[' {
			continue
		}
		end := matching(src, open)
		if end < 0 {
			return out
		}
		predicate, ok := strings.CutPrefix(strings.TrimSpace(string(src[open+1:end])), "cfg(")
		predicate, closed := strings.CutSuffix(predicate, ")")
		if !ok || !closed || evalCfg(predicate, cfg) {
			continue
		}
		last := lineOf(len(src) - 1)
		if !inner {
			last = lineOf(itemEnd(src, end+1) - 1)
		}
		out = append(out, lineRange{lineOf(i), last})
	}
	return out
}

// skipLiteral returns the index after the comment, string or character literal starting at i.
func skipLiteral(src []byte, i int) (int, bool) {
	rest := string(src[i:min(len(src), i+2)])
	switch {
	case rest == "//":
		end := strings.IndexByte(string(src[i:]), '\n')
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
	case src[i] == 'r' && (i == 0 || !isIdent(src[i-1])):
		hashes := 0
		j := i + 1
		for j < len(src) && src[j] == '#' {
			hashes++
			j++
		}
		if j < len(src) && src[j] == '"' {
			return endOfRawString(src, j+1, hashes), true
		}
	case src[i] == '\'':
		if i+2 < len(src) && src[i+1] == '\\' {
			end := strings.IndexByte(string(src[i+2:]), '\'')
			if end >= 0 {
				return i + 2 + end + 1, true
			}
		}
		if i+2 < len(src) && src[i+2] == '\'' {
			return i + 3, true
		}
	}
	return i, false
}

func isIdent(b byte) bool {
	return b == '_' || b >= '0' && b <= '9' || b >= 'a' && b <= 'z' || b >= 'A' && b <= 'Z'
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
	closing := "\"" + strings.Repeat("#", hashes)
	end := strings.Index(string(src[i:]), closing)
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

// itemEnd returns the index after the item starting at i: the end of its braces, or its first semicolon outside brackets.
func itemEnd(src []byte, i int) int {
	depth := 0
	for ; i < len(src); i++ {
		if next, ok := skipLiteral(src, i); ok {
			i = next - 1
			continue
		}
		switch src[i] {
		case '(', '[':
			depth++
		case ')', ']':
			depth--
		case '{':
			if depth == 0 {
				if end := matching(src, i); end >= 0 {
					return end + 1
				}
				return len(src)
			}
			depth++
		case '}':
			depth--
		case ';':
			if depth == 0 {
				return i + 1
			}
		}
	}
	return len(src)
}

// evalCfg evaluates a cfg predicate: a name, name = "value", all(...), any(...) or not(...). An option cfg lacks is false.
func evalCfg(predicate string, cfg map[string]bool) bool {
	p := strings.TrimSpace(predicate)
	for _, op := range []string{"all", "any", "not"} {
		args, ok := strings.CutPrefix(p, op+"(")
		if !ok || !strings.HasSuffix(args, ")") {
			continue
		}
		parts := splitTop(strings.TrimSuffix(args, ")"))
		switch op {
		case "not":
			return len(parts) == 1 && !evalCfg(parts[0], cfg)
		case "all":
			for _, part := range parts {
				if !evalCfg(part, cfg) {
					return false
				}
			}
			return true
		default:
			for _, part := range parts {
				if evalCfg(part, cfg) {
					return true
				}
			}
			return false
		}
	}
	name, value, ok := strings.Cut(p, "=")
	if !ok {
		return cfg[p]
	}
	return cfg[strings.TrimSpace(name)+"=\""+strings.Trim(strings.TrimSpace(value), "\"")+"\""]
}

// splitTop splits s at the commas outside parentheses and strings.
func splitTop(s string) []string {
	var parts []string
	depth, start := 0, 0
	inString := false
	for i := 0; i < len(s); i++ {
		switch {
		case s[i] == '"':
			inString = !inString
		case inString:
		case s[i] == '(':
			depth++
		case s[i] == ')':
			depth--
		case s[i] == ',' && depth == 0:
			parts = append(parts, strings.TrimSpace(s[start:i]))
			start = i + 1
		}
	}
	if rest := strings.TrimSpace(s[start:]); rest != "" {
		parts = append(parts, rest)
	}
	return parts
}
