#!/bin/sh
set -eu

fail() { printf '%s\n' "ojo installer: $*" >&2; exit 1; }
version=${OJO_VERSION:-0.1.0}
case "$version" in ''|*[!0-9A-Za-z.+-]*) fail 'invalid OJO_VERSION';; esac
case "$(uname -s):$(uname -m)" in
  Linux:x86_64|Linux:amd64) target=x86_64-unknown-linux-gnu;;
  *) fail 'no qualified binary for this platform; use the source installation in crates/ojo-tools/README.md';;
esac
install_dir=${OJO_INSTALL_DIR:-${HOME:?HOME is required}/.local/bin}
for tool in curl tar sha256sum awk install; do
  command -v "$tool" >/dev/null 2>&1 || fail "required command missing: $tool"
done
task_tmp=$(mktemp -d "${TMPDIR:-/tmp}/ojo-install.XXXXXXXX")
install_tmp=
trap 'rm -rf "$task_tmp"; if [ -n "$install_tmp" ]; then rm -f "$install_tmp"; fi' EXIT HUP INT TERM
tag=ojo-v$version
package=ojo-v$version-$target
archive=$package.tar.gz
base=https://github.com/oojuro/oojuro/releases/download/$tag
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
  --retry 2 --max-time 120 --output "$task_tmp/$archive" "$base/$archive"
curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
  --retry 2 --max-time 120 --output "$task_tmp/SHA256SUMS" "$base/SHA256SUMS"
awk -v name="$archive" '$2 == name {print; n++} END {if (n != 1) exit 1}' \
  "$task_tmp/SHA256SUMS" > "$task_tmp/selected.sha256" || fail 'release checksum is missing or ambiguous'
(cd "$task_tmp" && sha256sum -c selected.sha256) || fail 'release checksum mismatch'

# Refuse links, special files and any name outside the single package directory
# before extraction. A malformed release cannot write through a link in /tmp.
tar -tzf "$task_tmp/$archive" > "$task_tmp/members"
while IFS= read -r member; do
  case "$member" in /*|../*|*/../*|*/..) fail 'unsafe archive path';; esac
  case "$member" in "$package"|"$package/"|"$package/"*) ;; *) fail 'unexpected archive root';; esac
done < "$task_tmp/members"
tar -tvzf "$task_tmp/$archive" | awk 'substr($0,1,1) != "-" && substr($0,1,1) != "d" {bad=1} END {exit bad}' \
  || fail 'archive contains links or special files'
mkdir "$task_tmp/unpacked"
tar -xzf "$task_tmp/$archive" --no-same-owner --no-same-permissions -C "$task_tmp/unpacked"
payload=$task_tmp/unpacked/$package
test -f "$payload/bin/ojo" && test ! -L "$payload/bin/ojo" || fail 'release has no regular ojo binary'
for file in LICENSE NOTICE manifest.json; do
  test -f "$payload/$file" || fail "release is missing $file"
done
test -d "$payload/licenses" || fail 'release is missing dependency notices'
reported=$("$payload/bin/ojo" --version)
case "$reported" in "ojo $version "*) ;; *) fail 'binary version does not match requested release';; esac
printf '%s\n' "$reported"
mkdir -p -- "$install_dir"
install_dir=$(cd "$install_dir" && pwd -P)
share_dir=${install_dir%/*}/share/ojo/$version
mkdir -p -- "$share_dir"
cp -R "$payload/licenses" "$share_dir/"
cp "$payload/LICENSE" "$payload/NOTICE" "$payload/manifest.json" "$share_dir/"
install_tmp=$install_dir/.ojo-install-$$
install -m 755 "$payload/bin/ojo" "$install_tmp"
mv -f -- "$install_tmp" "$install_dir/ojo"
install_tmp=
printf 'Installed ojo %s to %s/ojo\n' "$version" "$install_dir"
printf 'Notices and source inventory: %s\n' "$share_dir"
