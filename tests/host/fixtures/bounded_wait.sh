#!/bin/bash

# Read one complete FIFO line without allowing a broken fixture to hang the
# whole test suite. The caller receives the line in BOUNDED_WAIT_LINE.
bounded_fifo_read() {
    local fifo=$1
    local description=$2
    local seconds=${3:-10}
    local line status

    if line=$(/usr/bin/perl -MTime::HiRes=alarm -e '
        my ($seconds, $path) = @ARGV;
        local $SIG{ALRM} = sub { exit 124 };
        alarm($seconds);
        open my $fifo, "<", $path or exit 126;
        my $line = <$fifo>;
        exit 125 unless defined $line;
        alarm(0);
        $line =~ s/\r?\n\z//;
        print $line;
    ' "$seconds" "$fifo"); then
        BOUNDED_WAIT_LINE=$line
        return 0
    else
        status=$?
    fi

    case "$status" in
        124)
            echo "FAIL: timed out after ${seconds}s waiting for $description" >&2
            ;;
        125)
            echo "FAIL: FIFO closed before $description: $fifo" >&2
            ;;
        *)
            echo "FAIL: could not read $description from FIFO: $fifo" >&2
            ;;
    esac
    return 1
}

# Succeed only when no FIFO line becomes readable before the deadline. A
# writer that closes early is a fixture failure, not proof that no event
# occurred. This uses the same portable Perl alarm as bounded_fifo_read so the
# result does not depend on Bash read -t support.
bounded_fifo_expect_no_line() {
    local fifo=$1
    local description=$2
    local seconds=${3:-1}
    local status

    if /usr/bin/perl -MTime::HiRes=alarm -e '
        my ($seconds, $path) = @ARGV;
        local $SIG{ALRM} = sub { exit 0 };
        alarm($seconds);
        open my $fifo, "<", $path or exit 126;
        my $line = <$fifo>;
        alarm(0);
        exit 125 unless defined $line;
        exit 123;
    ' "$seconds" "$fifo"; then
        return 0
    else
        status=$?
    fi

    case "$status" in
        123)
            echo "FAIL: received unexpected $description from FIFO: $fifo" >&2
            ;;
        125)
            echo "FAIL: FIFO closed while checking for $description: $fifo" >&2
            ;;
        *)
            echo "FAIL: could not check for $description from FIFO: $fifo" >&2
            ;;
    esac
    return 1
}

# Write one complete FIFO line without letting a missing reader block a test.
bounded_fifo_write() {
    local fifo=$1
    local line=$2
    local description=$3
    local seconds=${4:-10}
    local status

    if /usr/bin/perl -MTime::HiRes=alarm -e '
        my ($seconds, $path, $line) = @ARGV;
        local $SIG{ALRM} = sub { exit 124 };
        alarm($seconds);
        open my $fifo, ">", $path or exit 126;
        print {$fifo} $line, "\n" or exit 125;
        close $fifo or exit 125;
        alarm(0);
    ' "$seconds" "$fifo" "$line"; then
        return 0
    else
        status=$?
    fi

    case "$status" in
        124) echo "FAIL: timed out after ${seconds}s writing $description" >&2 ;;
        *) echo "FAIL: could not write $description to FIFO: $fifo" >&2 ;;
    esac
    return 1
}

# Poll an observable condition. The interval only limits CPU use; success is
# decided by the condition, and failure is decided by the explicit deadline.
bounded_wait_until() {
    local seconds=$1
    local description=$2
    shift 2
    local deadline=$((SECONDS + seconds))

    while ! "$@"; do
        if [ "$SECONDS" -ge "$deadline" ]; then
            echo "FAIL: timed out after ${seconds}s waiting for $description" >&2
            return 1
        fi
        /bin/sleep 0.05
    done
}

# Return success after a direct child has exited or become waitable as a
# zombie. This avoids kill -0 loops that mistake an unreaped child for work
# that can still make progress.
bounded_child_is_finished() {
    local pid=$1
    local state
    case "$pid" in
        ''|*[!0-9]*) return 1 ;;
    esac
    state=$(ps -p "$pid" -o state= 2>/dev/null || true)
    case "$state" in
        ''|*Z*) return 0 ;;
    esac
    return 1
}

# Stop and reap only a PID that the calling test started directly. TERM gets a
# bounded grace period before KILL, and KILL is also checked before wait.
bounded_stop_child() {
    local pid=$1
    local description=$2
    local seconds=${3:-5}
    case "$pid" in
        ''|*[!0-9]*)
            echo "FAIL: invalid $description pid: $pid" >&2
            return 1
            ;;
    esac
    if ! bounded_child_is_finished "$pid"; then
        kill -TERM "$pid" 2>/dev/null || true
        if ! bounded_wait_until "$seconds" "$description to stop after TERM" \
            bounded_child_is_finished "$pid"; then
            kill -KILL "$pid" 2>/dev/null || true
            if ! bounded_wait_until "$seconds" \
                "$description to stop after KILL" \
                bounded_child_is_finished "$pid"; then
                return 1
            fi
        fi
    fi
    wait "$pid" 2>/dev/null || true
}
