#!/bin/sh

if ! which rustup; then
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
        PATH="$PATH:$HOME/.cargo/bin"
fi

cargo build --release
sudo systemctl restart raspberry-bot
