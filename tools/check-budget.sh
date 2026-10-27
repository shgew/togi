#!/usr/bin/env bash
set -euo pipefail

if [[ ${TOGI_JOBS+x} ]]; then
    if [[ ! $TOGI_JOBS =~ ^[1-9][0-9]*$ ]]; then
        echo 'TOGI_JOBS must be a positive integer' >&2
        exit 1
    fi
    printf '%s\n' "$TOGI_JOBS"
    exit 0
fi

case $(uname -s) in
    Linux)
        cpus=$(nproc)
        while read -r key value unit; do
            if [[ $key == MemTotal: ]]; then
                memory_jobs=$((value / 2097152))
                break
            fi
        done < /proc/meminfo
        ;;
    Darwin)
        cpus=$(sysctl -n hw.ncpu)
        memory_jobs=$(($(sysctl -n hw.memsize) / 2147483648))
        ;;
    *)
        echo 'check budget needs Linux or macOS; set TOGI_JOBS on other hosts' >&2
        exit 1
        ;;
esac

jobs=$cpus
if ((memory_jobs < jobs)); then jobs=$memory_jobs; fi
if ((jobs < 1)); then jobs=1; fi
printf '%s\n' "$jobs"
