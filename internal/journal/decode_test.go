package journal

func decode(line []byte) (Event, error) {
	return decodeEvent(line, false)
}
