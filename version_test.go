package togi

import (
	"runtime/debug"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestResolveRevision(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name     string
		explicit string
		settings []debug.BuildSetting
		ok       bool
		want     string
	}{
		{
			name:     "explicit overrides VCS",
			explicit: "release-revision",
			settings: []debug.BuildSetting{{Key: "vcs.revision", Value: "123456789abcdef"}, {Key: "vcs.modified", Value: "true"}},
			ok:       true,
			want:     "release-revision",
		},
		{
			name:     "explicit dev overrides VCS",
			explicit: "dev",
			settings: []debug.BuildSetting{{Key: "vcs.revision", Value: "123456789abcdef"}},
			ok:       true,
			want:     "dev",
		},
		{
			name:     "clean checkout",
			settings: []debug.BuildSetting{{Key: "vcs.revision", Value: "123456789abcdef"}, {Key: "vcs.modified", Value: "false"}},
			ok:       true,
			want:     "1234567",
		},
		{
			name:     "dirty checkout",
			settings: []debug.BuildSetting{{Key: "vcs.modified", Value: "true"}, {Key: "vcs.revision", Value: "abcdef123456789"}},
			ok:       true,
			want:     "abcdef1-dirty",
		},
		{
			name:     "short revision",
			settings: []debug.BuildSetting{{Key: "vcs.revision", Value: "abc123"}},
			ok:       true,
			want:     "abc123",
		},
		{
			name:     "modified without revision",
			settings: []debug.BuildSetting{{Key: "vcs.modified", Value: "true"}},
			ok:       true,
			want:     "dev",
		},
		{
			name: "no VCS settings",
			ok:   true,
			want: "dev",
		},
		{
			name: "no build information",
			want: "dev",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			readBuildInfo := func() (*debug.BuildInfo, bool) {
				if tt.explicit != "" {
					t.Fatal("explicit revision must not read build information")
				}
				if !tt.ok {
					return nil, false
				}
				return &debug.BuildInfo{Settings: tt.settings}, true
			}
			if diff := cmp.Diff(tt.want, resolveRevision(tt.explicit, readBuildInfo)); diff != "" {
				t.Fatalf("revision (-want +got):\n%s", diff)
			}
		})
	}
}
