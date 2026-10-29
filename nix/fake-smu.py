#!/usr/bin/env python3
"""VM-only synchronous ryzen_smu sysfs protocol; never linked into togi.

The driver's write returns only after its response has changed. direct_io
prevents the kernel page cache from hiding that response on the immediate read.
The fixture presents one complete eight-slot CCD, as required by slot validation.
"""

import errno
import json
from pathlib import Path
import stat
import struct
import sys

from mfusepy import FUSE, FuseOSError, Operations, fuse_get_context


class Smu(Operations):
    def __init__(self, state):
        self.log = Path(state) / "protocol.jsonl"
        self.offsets = [0] * 8
        self.files = {
            "/proc/cpuinfo": b"cpu family : 26\nmodel : 68\nmodel name : Zen VM\nmicrocode : 0x1\n",
            "/sys/class/dmi/id/bios_version": b"vm\n",
            "/sys/class/dmi/id/board_vendor": b"VM\n",
            "/sys/class/dmi/id/board_name": b"VM\n",
            "/sys/kernel/ryzen_smu_drv/codename": b"23\n",
            "/sys/kernel/ryzen_smu_drv/drv_version": b"0.1\n",
            "/sys/kernel/ryzen_smu_drv/version": b"57.13\n",
            "/sys/kernel/ryzen_smu_drv/smu_args": bytes(24),
            "/sys/kernel/ryzen_smu_drv/rsmu_cmd": struct.pack("<I", 1),
            "/sys/kernel/ryzen_smu_drv/smn": bytes(4),
        }
        for core in range(8):
            base = f"/sys/devices/system/cpu/cpu{core}"
            for name, value in {
                "topology/core_id": core,
                "topology/physical_package_id": 0,
                "topology/die_id": 0,
                "cache/index3/id": 0,
            }.items():
                self.files[f"{base}/{name}"] = f"{value}\n".encode()
        self.dirs = {"/"}
        for name in self.files:
            self.dirs.update(str(parent) for parent in Path(name).parents)

    def getattr(self, path, fh=None):
        if path in self.dirs:
            return dict(st_mode=stat.S_IFDIR | 0o755, st_nlink=2)
        if path in self.files:
            return dict(st_mode=stat.S_IFREG | 0o600, st_nlink=1,
                        st_size=len(self.files[path]))
        raise FuseOSError(errno.ENOENT)

    def readdir(self, path, fh):
        prefix = path.rstrip("/") + "/"
        return [".", ".."] + sorted({
            name[len(prefix):].split("/", 1)[0]
            for name in self.files if name.startswith(prefix)
        })

    def open(self, path, flags):
        if path not in self.files:
            raise FuseOSError(errno.ENOENT)
        return 0

    def read(self, path, size, offset, fh):
        return self.files[path][offset:offset + size]

    def record(self, **entry):
        entry["pid"] = fuse_get_context()[2]
        journal = Path("/var/lib/togi/events.jsonl")
        if journal.exists():
            rows = journal.read_text().splitlines()
            if rows:
                latest = json.loads(rows[-1])
                entry["journal_seq"] = latest["seq"]
                entry["journal_kind"] = latest["kind"]
        with self.log.open("a") as log:
            log.write(json.dumps(entry) + "\n")

    def write(self, path, data, offset, fh):
        name = Path(path).name
        driver = "/sys/kernel/ryzen_smu_drv/"
        if path != driver + name or offset != 0:
            raise FuseOSError(errno.EINVAL)
        if name == "smu_args" and len(data) == 24:
            self.files[path] = data
        elif name == "smn" and len(data) == 4:
            address, = struct.unpack("<I", data)
            values = {0x03B10570: 1, 0x03B10524: 0x6E, 0x304A03DC: 0}
            if address not in values:
                raise FuseOSError(errno.EINVAL)
            self.files[path] = struct.pack("<I", values[address])
            self.record(operation="smn", address=address, value=values[address])
        elif name == "rsmu_cmd" and len(data) == 4:
            command, = struct.unpack("<I", data)
            args = list(struct.unpack("<6I", self.files[driver + "smu_args"]))
            argument = args[0]
            core = (argument >> 20) & 0xFF
            response = 0
            if command == 0xD5 and argument == core << 20 and core < 8:
                response = self.offsets[core] & 0xFFFF
                self.record(operation="read", command=command, core=core,
                            value=self.offsets[core])
            elif command in (0x06, 0x07):
                # Refuse non-stock commands rather than let a test pass on a fake
                # that silently discards an unsafe production write.
                if (command == 0x07 and argument != 0) or (
                    command == 0x06 and (core >= 8 or argument != core << 20)
                ):
                    self.record(operation="refused-write", command=command, argument=argument)
                    raise FuseOSError(errno.EPERM)
                if command == 0x07:
                    self.offsets = [0] * 8
                else:
                    self.offsets[core] = 0
                self.record(operation="write", command=command, argument=argument,
                            targets=list(self.offsets))
            elif command == 0x6E:
                response = 5000
                self.record(operation="boost-limit", command=command, value=response)
            else:
                self.record(operation="unexpected-command", command=command, argument=argument)
                raise FuseOSError(errno.EINVAL)
            self.files[driver + "smu_args"] = struct.pack("<6I", response, 0, 0, 0, 0, 0)
            self.files[path] = struct.pack("<I", 1)
        else:
            raise FuseOSError(errno.EINVAL)
        return len(data)


if __name__ == "__main__":
    FUSE(Smu(sys.argv[2]), sys.argv[1], foreground=True, nothreads=True,
         direct_io=True, attr_timeout=0, entry_timeout=0, negative_timeout=0)
