#!/bin/sh

set -eu

artifacts=$(
    find /tmp -maxdepth 1 -name 'fork_test*' -print 2>/dev/null
    if [ "${TMPDIR:-/tmp}" != /tmp ]; then
        find "${TMPDIR}" -maxdepth 1 -name 'fork_test*' -print 2>/dev/null
    fi
)
if [ -n "${artifacts}" ]; then
    echo "test filesystem artifacts remain:"
    echo "${artifacts}"
    exit 1
fi

cores=$(find . -maxdepth 1 -name '*.core' -print 2>/dev/null)
if [ -n "${cores}" ]; then
    echo "test core files remain:"
    echo "${cores}"
    exit 1
fi

pids=$(pgrep -f '[f]ork.*/target/(debug|release)' || true)
if [ -n "${pids}" ]; then
    echo "test processes remain:"
    echo "${pids}"
    exit 1
fi
