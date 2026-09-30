package togi

import (
	_ "embed"
	"runtime/debug"
	"strings"
)

//go:embed version.txt
var version string

var rev string

var buildRev = resolveRevision(rev, debug.ReadBuildInfo)

func Version() string { return strings.TrimSpace(version) }

func Rev() string { return buildRev }

func String() string { return Version() + "+" + Rev() }

func resolveRevision(explicit string, readBuildInfo func() (*debug.BuildInfo, bool)) string {
	if explicit != "" {
		return explicit
	}
	info, ok := readBuildInfo()
	if !ok {
		return "dev"
	}
	var revision string
	var modified bool
	for _, setting := range info.Settings {
		switch setting.Key {
		case "vcs.revision":
			revision = setting.Value
		case "vcs.modified":
			modified = setting.Value == "true"
		}
	}
	if revision == "" {
		return "dev"
	}
	revision = revision[:min(7, len(revision))]
	if modified {
		revision += "-dirty"
	}
	return revision
}
