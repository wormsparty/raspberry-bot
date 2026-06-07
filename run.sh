#!/bin/sh

cargo build --release
systemctl restart xfiles-bot
