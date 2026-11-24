#!/usr/bin/env bash
# The session in docs/media/kv-cli-demo-v2.gif: the same commands as
# session.sh, holding each result long enough for storyboard-v2.json's zooms
# and captions. record-v2.sh runs it inside asciinema, in a scratch copy of
# project/: each command is typed out, then run for real, so every line of
# output is kv-cli's own.
set -u

PROMPT=$'\e[1;34macme-pipelines\e[0m \e[2m$\e[0m '

# Print $1 a character at a time, as if typed.
type_out() {
    local text=$1 i
    for ((i = 0; i < ${#text}; i++)); do
        printf '%s' "${text:i:1}"
        sleep 0.04
    done
}

# Type a command at the prompt, run it, then hold its output for $2 seconds.
run() {
    printf '%s' "$PROMPT"
    sleep 0.6
    type_out "$1"
    sleep 0.4
    printf '\n'
    eval "$1"
    sleep "${2:-2}"
}

# A new scene: the next command starts at the top of a clean screen, so each
# step is seen whole.
scene() {
    printf '\e[H\e[2J'
}

scene
sleep 2.8
run "kv-cli init" 7
run "cat jobs/load_orders.py" 6.5
scene
run "kv-cli audit" 10.5
scene
run "kv-cli fix" 7.5
scene
run "git diff" 4.5
scene
run "kv-cli audit" 4.5
printf '%s' "$PROMPT"
sleep 4
