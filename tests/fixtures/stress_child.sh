#!/bin/bash
# Synthetic child for recorder stress tests.
#
# One printf call, single line, no newlines: pattern + trailing bytes
# leave the child as a single write syscall. PTY delivers atomically,
# the drainer reads all bytes in one chunk, fires the watch on the
# in-chunk pattern match, and adds the whole chunk to the buffer
# under one mutex acquisition. By the time the recorder's `consume`
# runs, the buffer contains pattern + trailing — independent of
# scheduling.
#
# This makes the wait_for cutoff race deterministically observable:
# - With "consume returns whole buffer" (current racy primitive),
#   the wait_for event always contains both pattern AND trailing.
# - With "consume returns up to pattern_end" (corrected primitive),
#   the wait_for event always contains only the pattern.
#
# Process-agnostic: no docker, no domain knowledge. Bash printf only.

set -u

printf 'PROMPT$ payload-extra-bytes-here'
