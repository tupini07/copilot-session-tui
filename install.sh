#!/usr/bin/env bash
set -eu

repository="tupini07/copilot-session-tui"
asset="copilot-session-tui-x86_64-unknown-linux-gnu.tar.gz"
install_dir="${CST_INSTALL_DIR:-"$HOME/.local/bin"}"
skip_shell_init="${CST_NO_SHELL_INIT:-0}"

if [ -n "${ZSH_VERSION:-}" ]; then
    shell_name="zsh"
    profile="${CST_PROFILE:-"${ZDOTDIR:-"$HOME"}/.zshrc"}"
elif [ -n "${BASH_VERSION:-}" ]; then
    shell_name="bash"
    profile="${CST_PROFILE:-"$HOME/.bashrc"}"
else
    echo "Run this installer with bash or zsh." >&2
    exit 1
fi

case "$(uname -s)" in
    Linux) ;;
    *)
        echo "CST currently publishes a prebuilt POSIX binary for Linux x64 only." >&2
        exit 1
        ;;
esac

case "$(uname -m)" in
    x86_64|amd64) ;;
    *)
        echo "CST currently publishes a Linux x64 binary only." >&2
        exit 1
        ;;
esac

for command in curl tar install; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "Required command not found: $command" >&2
        exit 1
    fi
done

temporary="$(mktemp -d "${TMPDIR:-/tmp}/cst-install.XXXXXXXX")"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

archive="$temporary/$asset"
asset_url="https://github.com/$repository/releases/latest/download/$asset"
curl --proto '=https' --tlsv1.2 -fLsS "$asset_url" -o "$archive"

checksums="$temporary/SHA256SUMS"
if curl --proto '=https' --tlsv1.2 -fLsS \
    "https://github.com/$repository/releases/latest/download/SHA256SUMS" \
    -o "$checksums" 2>/dev/null; then
    expected="$(awk -v asset="$asset" '$2 == asset || $2 == "*" asset { print $1; exit }' "$checksums")"
    if [ -z "$expected" ]; then
        echo "SHA256SUMS does not contain $asset." >&2
        exit 1
    fi
    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$archive" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$archive" | awk '{print $1}')"
    else
        echo "SHA256SUMS is available, but sha256sum/shasum is not installed." >&2
        exit 1
    fi
    if [ "$actual" != "$expected" ]; then
        echo "SHA-256 mismatch for $asset." >&2
        exit 1
    fi
else
    echo "warning: this older release does not publish SHA256SUMS; relying on HTTPS" >&2
fi

tar -xzf "$archive" -C "$temporary"
binary="$temporary/copilot-session-tui"
if [ ! -f "$binary" ]; then
    echo "$asset did not contain copilot-session-tui." >&2
    exit 1
fi

mkdir -p "$install_dir"
staged_binary="$install_dir/.copilot-session-tui.installing.$$"
install -m 0755 "$binary" "$staged_binary"
mv -f "$staged_binary" "$install_dir/copilot-session-tui"

if [ "$skip_shell_init" != "1" ]; then
    start_marker="# >>> copilot-session-tui >>>"
    if [ ! -f "$profile" ] || ! grep -Fq "$start_marker" "$profile"; then
        profile_dir="$(dirname "$profile")"
        mkdir -p "$profile_dir"
        profile_install_dir="$(printf '%s' "$install_dir" | sed 's/[\\`"$]/\\&/g')"
        {
            printf '\n%s\n' "$start_marker"
            printf 'export PATH="%s:$PATH"\n' "$profile_install_dir"
            printf 'eval "$(copilot-session-tui init %s)"\n' "$shell_name"
            printf '%s\n' "# <<< copilot-session-tui <<<"
        } >> "$profile"
    fi
fi

"$install_dir/copilot-session-tui" --version
echo "Installed CST to $install_dir/copilot-session-tui"
if [ "$skip_shell_init" = "1" ]; then
    echo "Shell integration skipped because CST_NO_SHELL_INIT=1."
else
    echo "$shell_name integration is configured. Restart $shell_name, then run: cst"
fi
