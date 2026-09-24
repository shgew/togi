package smu

import (
	"encoding/binary"
	"fmt"
	"os"
	"path/filepath"
	"sync"
)

type Mailbox interface {
	Command(cmd uint32, args [6]uint32) ([6]uint32, error)
	ReadSMN(addr uint32) (uint32, error)
}

type sysfs struct {
	dir string
	mu  sync.Mutex
}

func Sysfs(root string) Mailbox {
	dir := filepath.Join(root, "sys/kernel/ryzen_smu_drv")
	if info, err := os.Stat(dir); err != nil || !info.IsDir() {
		return nil
	}
	return &sysfs{dir: dir}
}

func (s *sysfs) Command(cmd uint32, args [6]uint32) ([6]uint32, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	var data [24]byte
	for i, arg := range args {
		binary.LittleEndian.PutUint32(data[i*4:], arg)
	}
	if err := s.write("smu_args", data[:]); err != nil {
		return [6]uint32{}, err
	}
	var command [4]byte
	binary.LittleEndian.PutUint32(command[:], cmd)
	if err := s.write("rsmu_cmd", command[:]); err != nil {
		return [6]uint32{}, err
	}
	status, err := s.read("rsmu_cmd", 4)
	if err != nil {
		return [6]uint32{}, err
	}
	if code := binary.LittleEndian.Uint32(status); code != 1 {
		name := map[uint32]string{0xff: "failed", 0xfe: "unknown command", 0xfd: "rejected: prerequisite", 0xfc: "rejected: busy"}[code]
		if name == "" {
			name = "unknown status"
		}
		return [6]uint32{}, fmt.Errorf("RSMU command 0x%x: status 0x%02x (%s)", cmd, code, name)
	}
	response, err := s.read("smu_args", 24)
	if err != nil {
		return [6]uint32{}, err
	}
	var result [6]uint32
	for i := range result {
		result[i] = binary.LittleEndian.Uint32(response[i*4:])
	}
	return result, nil
}

func (s *sysfs) ReadSMN(addr uint32) (uint32, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	var data [4]byte
	binary.LittleEndian.PutUint32(data[:], addr)
	if err := s.write("smn", data[:]); err != nil {
		return 0, err
	}
	response, err := s.read("smn", 4)
	if err != nil {
		return 0, err
	}
	return binary.LittleEndian.Uint32(response), nil
}

func (s *sysfs) write(name string, data []byte) error {
	path := filepath.Join(s.dir, name)
	f, err := os.OpenFile(path, os.O_WRONLY, 0)
	if err != nil {
		return fmt.Errorf("open %s: %w", path, err)
	}
	n, err := f.Write(data)
	if err == nil && n != len(data) {
		err = fmt.Errorf("short write: %d of %d bytes", n, len(data))
	}
	if closeErr := f.Close(); err == nil {
		err = closeErr
	}
	if err != nil {
		return fmt.Errorf("write %s: %w", path, err)
	}
	return nil
}

func (s *sysfs) read(name string, size int) ([]byte, error) {
	path := filepath.Join(s.dir, name)
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", path, err)
	}
	if len(data) != size {
		return nil, fmt.Errorf("read %s: got %d bytes, want %d", path, len(data), size)
	}
	return data, nil
}
