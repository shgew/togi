package main

func cpuid(eaxArg, ecxArg uint32) (eax, ebx, ecx, edx uint32)

func xgetbv() (eax, edx uint32)

func isSet(word, bit uint32) bool {
	return word&bit != 0
}

// detectAVXFMA mirrors the detection in Go's internal/cpu, whose result
// math.useFMA combines as HasAVX && HasFMA.
func detectAVXFMA() (avx, fma bool) {
	maxID, _, _, _ := cpuid(0, 0)
	if maxID < 1 {
		return false, false
	}
	_, _, ecx1, _ := cpuid(1, 0)
	osxsave := isSet(ecx1, 1<<27)
	osAVX := false
	if osxsave {
		eax, _ := xgetbv()
		osAVX = isSet(eax, 1<<1) && isSet(eax, 1<<2)
	}
	avx = isSet(ecx1, 1<<28) && osAVX
	fma = isSet(ecx1, 1<<12) && avx && osxsave
	return avx, fma
}
