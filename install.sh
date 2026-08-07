#!/bin/sh
#
# Installs Lucida: picks the right release binary, verifies its published
# checksum, and puts it somewhere on your PATH.
#
#   curl -fsSL https://raw.githubusercontent.com/Artificial-Humanity/Lucida/main/install.sh | sh
#
# Or read it first and run it yourself, which is the better habit:
#
#   curl -fsSL .../install.sh -o install.sh && less install.sh && sh install.sh
#
# EVERYTHING IS IN A FUNCTION, CALLED ON THE LAST LINE. That is not style, it is
# the one real hazard of piping a script into a shell: if the connection drops
# mid-transfer, `sh` runs whatever bytes arrived — which for a straight-line
# script means executing the first half of it. Half of `rm -rf "$dir"` with the
# variable unset is the kind of thing that ends badly. Defined-then-called, a
# truncated file defines some functions and does nothing at all.
#
# Settings:
#   LUCIDA_INSTALL_DIR   where to put it (default: ~/.local/bin)
#   LUCIDA_VERSION       a tag to pin, e.g. v0.9.0 (default: the latest release)
#   GITHUB_TOKEN         used if set, purely to avoid the unauthenticated
#                        rate limit — no scopes are needed for public releases

set -eu

REPO="Artificial-Humanity/Lucida"
RELEASES_PAGE="https://github.com/$REPO/releases/latest"

say() { printf '%s\n' "$*"; }
warn() { printf '%s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || die "$1 is required and was not found."
}

# The asset for this machine. Unknown platforms are an error naming the
# releases page rather than a guess that downloads the wrong architecture.
asset_for_platform() {
    os=$(uname -s)
    arch=$(uname -m)

    case "$os" in
        Darwin)
            # One universal binary covers both Apple architectures, which is why
            # arch is not consulted here.
            printf 'lucida-%s-macos-universal' "$1"
            ;;
        Linux)
            case "$arch" in
                x86_64 | amd64) printf 'lucida-%s-x86_64-linux-musl' "$1" ;;
                *) die "no release binary is published for Linux $arch. See $RELEASES_PAGE" ;;
            esac
            ;;
        *)
            die "no release binary is published for $os. On Windows use install.ps1; otherwise see $RELEASES_PAGE"
            ;;
    esac
}

# The tag to install. The API is asked rather than the version being guessed,
# so this script has one fewer fact to keep in sync with the repository.
resolve_tag() {
    if [ -n "${LUCIDA_VERSION:-}" ]; then
        printf '%s' "$LUCIDA_VERSION"
        return
    fi

    api="https://api.github.com/repos/$REPO/releases/latest"
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        body=$(curl -fsSL -H "Authorization: Bearer $GITHUB_TOKEN" "$api") ||
            die "could not reach $api. See $RELEASES_PAGE"
    else
        body=$(curl -fsSL "$api") ||
            die "could not reach $api — if this is a rate limit it clears within the hour. See $RELEASES_PAGE"
    fi

    # One small parse rather than a jq dependency: the asset URL is then built
    # from the tag, since release asset URLs have a fixed shape.
    tag=$(printf '%s' "$body" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
    [ -n "$tag" ] || die "could not read a version from $api. See $RELEASES_PAGE"
    printf '%s' "$tag"
}

# Verified, not merely downloaded — the easiest install path must not also be
# the least checked one. No flag skips this: a checksum tool being absent is a
# reason to install by hand, not to install unverified.
verify() {
    file=$1
    published=$2

    expected=$(cut -d' ' -f1 <"$published")
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$file" | cut -d' ' -f1)
    elif command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "$file" | cut -d' ' -f1)
    else
        die "no sha256sum or shasum found, so the download cannot be verified. Install one, or take the binary from $RELEASES_PAGE"
    fi

    [ "$actual" = "$expected" ] ||
        die "the download does not match its published checksum, so it was not installed.
  expected $expected
  got      $actual"
}

main() {
    need curl
    need uname

    dir=${LUCIDA_INSTALL_DIR:-$HOME/.local/bin}

    tag=$(resolve_tag)
    version=${tag#v}
    asset=$(asset_for_platform "$version")
    base="https://github.com/$REPO/releases/download/$tag"

    say "Installing lucida $version to $dir"

    tmp=$(mktemp -d) || die "could not create a temporary directory"
    trap 'rm -rf "$tmp"' EXIT INT TERM

    curl -fsSL -o "$tmp/$asset" "$base/$asset" ||
        die "could not download $base/$asset. See $RELEASES_PAGE"
    curl -fsSL -o "$tmp/$asset.sha256" "$base/$asset.sha256" ||
        die "could not download the checksum for $asset. See $RELEASES_PAGE"

    verify "$tmp/$asset" "$tmp/$asset.sha256"
    say "Checksum verified."

    mkdir -p "$dir" || die "could not create $dir"
    chmod +x "$tmp/$asset"
    mv -f "$tmp/$asset" "$dir/lucida" || die "could not write to $dir"

    # macOS quarantine is set by browsers and LaunchServices, not by curl, so a
    # binary arriving this way needs no `xattr -d` — unlike one downloaded from
    # the releases page in a browser. Worth knowing rather than assuming.
    say ""
    say "$("$dir/lucida" --version) installed at $dir/lucida"

    case ":$PATH:" in
        *":$dir:"*)
            say "Run \`lucida --help\` to get started."
            ;;
        *)
            # Said rather than done: editing someone's shell profile from a
            # piped script is a larger liberty than installing a binary they
            # asked for, and the line differs by shell.
            warn ""
            warn "note: $dir is not on your PATH. Add it:"
            warn ""
            warn "  export PATH=\"$dir:\$PATH\""
            ;;
    esac
}

main "$@"
