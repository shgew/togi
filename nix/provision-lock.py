import fcntl
import grp
import os
import stat
import sys

path, group = sys.argv[1:]
parent = os.stat(os.path.dirname(path))
if parent.st_uid != 0 or parent.st_mode & 0o022:
    raise SystemExit("lock parent must be root-owned and not writable by other users")
gid = 0 if group == "-" else grp.getgrnam(group).gr_gid
fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
try:
    info = os.fstat(fd)
    if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_nlink != 1:
        raise SystemExit("refusing an unsafe or foreign lock inode")
    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    os.fchmod(fd, 0o600)
    os.fchown(fd, 0, gid)
    if group != "-":
        os.fchmod(fd, 0o660)
finally:
    os.close(fd)
