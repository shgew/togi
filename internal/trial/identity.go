package trial

import (
	"errors"
	"fmt"
	"os/user"
	"path/filepath"
	"strconv"
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
