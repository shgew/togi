//go:build !amd64

package main

func detectAVXFMA() (avx, fma bool) {
	return false, false
}
