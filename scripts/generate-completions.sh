#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

output=target/completions
mkdir -p "$output"
cargo build --quiet --bin wizlight
binary=target/debug/wizlight

"$binary" completions bash > "$output/wizlight.bash"
"$binary" completions zsh > "$output/_wizlight"
"$binary" completions fish > "$output/wizlight.fish"
"$binary" completions powershell > "$output/wizlight.ps1"
"$binary" completions elvish > "$output/wizlight.elv"
