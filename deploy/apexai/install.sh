#!/usr/bin/env bash

set -euo pipefail

apexai_home=${APEXAI_HOME:-$HOME/.apexai}
install_dir=$apexai_home/bin
nfs_launcher=${APEXAI_NFS_LAUNCHER:-/x/eng/apex/apexai/apexai.sh}

if [[ ! -x "$nfs_launcher" ]]; then
    printf 'apexai: NFS launcher not found at %s\n' "$nfs_launcher" >&2
    exit 1
fi

mkdir -p "$install_dir"
ln -sfn "$nfs_launcher" "$install_dir/apexai"

_apx_marker='# ApexAI CLI'
_apx_sh_path='export PATH="$HOME/.apexai/bin:$PATH"'

_apx_append_sh_path() {
    local file=$1
    [[ -f "$file" ]] || return 0
    grep -q '\.apexai/bin' "$file" 2>/dev/null && return 0
    printf '\n%s\n%s\n' "$_apx_marker" "$_apx_sh_path" >> "$file"
}

_apx_append_sh_path "$HOME/.bashrc"
_apx_append_sh_path "$HOME/.zshrc"

_apx_login_rc=
for _apx_file in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
    if [[ -f "$_apx_file" ]]; then
        _apx_login_rc=$_apx_file
        break
    fi
done
if [[ -n "$_apx_login_rc" ]]; then
    _apx_append_sh_path "$_apx_login_rc"
else
    printf '%s\n[ -f "$HOME/.bashrc" ] && . "$HOME/.bashrc"\n%s\n' \
        "$_apx_marker" "$_apx_sh_path" > "$HOME/.bash_profile"
fi

_apx_append_sh_path "$HOME/.zprofile"

if [[ -f "$HOME/.cshrc" ]] && ! grep -q '\.apexai/bin' "$HOME/.cshrc" 2>/dev/null; then
    printf '\n%s\nif ( -d ~/.apexai/bin ) then\n    if ( ! $?path ) set path = ()\n    set path = ( ~/.apexai/bin $path )\nendif\n' \
        "$_apx_marker" >> "$HOME/.cshrc"
fi

printf 'ApexAI installed: %s -> %s\n' "$install_dir/apexai" "$nfs_launcher"
