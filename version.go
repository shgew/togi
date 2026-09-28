package togi

import (
	_ "embed"
	"strings"
)

//go:embed version.txt
var version string

var rev = "dev"

func Version() string { return strings.TrimSpace(version) }

func Rev() string { return rev }

func String() string { return Version() + "+" + Rev() }
