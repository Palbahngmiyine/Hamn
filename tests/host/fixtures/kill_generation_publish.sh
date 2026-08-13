#!/bin/bash

# Sourced through BASH_ENV. Kill at the one externally visible commit point:
# immediately before BINDIR/hamn is renamed, or on the first command after it.
trap 'publish_command=$BASH_COMMAND
    if [ "${HAMN_KILL_GENERATION_PUBLISH:-}" = after ] &&
        [ "${HAMN_GENERATION_COMMITTED:-0}" = 1 ]; then
        trap - DEBUG
        kill -KILL "$$"
    fi
    case "$publish_command" in
    /bin/mv\ -[fn]\ \"\$hamn_link_temp\"\ \"\$HAMN_PATH\")
        if [ "${HAMN_KILL_GENERATION_PUBLISH:-}" = before ]; then
            trap - DEBUG
            kill -KILL "$$"
        fi
        HAMN_GENERATION_COMMITTED=1
        ;;
    esac' DEBUG
