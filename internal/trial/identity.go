package trial

import (
	"errors"
	"fmt"
	"os"
	"os/user"
	"path/filepath"
	"strconv"
	"syscall"
)

type Identity struct {
	UID, GID uint32
}

func LookupIdentity(name string) (Identity, error) {
	return lookupIdentity(name, user.Lookup)
}

func lookupIdentity(name string, lookup func(string) (*user.User, error)) (Identity, error) {
	if name == "" {
		return Identity{}, errors.New("backend_user not configured")
	}
	u, err := lookup(name)
	if err != nil {
		return Identity{}, fmt.Errorf("backend_user %q: %w", name, err)
	}
	uid, err := strconv.ParseUint(u.Uid, 10, 32)
	if err != nil {
		return Identity{}, fmt.Errorf("backend_user %q uid: %w", name, err)
	}
	gid, err := strconv.ParseUint(u.Gid, 10, 32)
	if err != nil {
		return Identity{}, fmt.Errorf("backend_user %q gid: %w", name, err)
	}
	id := Identity{UID: uint32(uid), GID: uint32(gid)}
	if err := id.validate(); err != nil {
		return Identity{}, fmt.Errorf("backend_user %q: %w", name, err)
	}
	return id, nil
}

func (id Identity) validate() error {
	if id.UID == 0 || id.GID == 0 || id.UID == ^uint32(0) || id.GID == ^uint32(0) {
		return errors.New("requires non-root uid and gid within [1, 4294967294]")
	}
	return nil
}

func ownDirectory(dir string, files []string, id Identity, chown func(string, int, int) error) error {
	for _, name := range files {
		path := filepath.Join(dir, name)
		if err := chown(path, int(id.UID), int(id.GID)); err != nil {
			return fmt.Errorf("own backend input %s: %w", path, err)
		}
	}
	if err := chown(dir, int(id.UID), int(id.GID)); err != nil {
		return fmt.Errorf("own backend directory %s: %w", dir, err)
	}
	return nil
}

func checkTraversal(dir string, id Identity) error {
	path, err := filepath.EvalSymlinks(dir)
	if err != nil {
		return fmt.Errorf("resolve backend directory %s: %w", dir, err)
	}
	for {
		info, err := os.Stat(path)
		if err != nil {
			return fmt.Errorf("stat backend ancestor %s: %w", path, err)
		}
		stat, ok := info.Sys().(*syscall.Stat_t)
		if !ok {
			return fmt.Errorf("read backend ancestor ownership %s", path)
		}
		permission := os.FileMode(0001)
		if stat.Uid == id.UID {
			permission = 0100
		} else if stat.Gid == id.GID {
			permission = 0010
		}
		if !info.IsDir() || info.Mode().Perm()&permission == 0 {
			return fmt.Errorf("backend uid %d gid %d cannot traverse %s", id.UID, id.GID, path)
		}
		parent := filepath.Dir(path)
		if parent == path {
			return nil
		}
		path = parent
	}
}
