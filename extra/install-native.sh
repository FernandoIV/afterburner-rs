#!/usr/bin/env bash
# Run after cargo build --release --offline. No package downloads or passwords.
set -euo pipefail
if [[ $EUID != 0 ]]; then
    echo "Run: sudo bash extra/install-native.sh DESKTOP_USER" >&2
    exit 1
fi
user=${1:-${SUDO_USER:-}}
[[ "$user" =~ ^[a-z_][a-z0-9_-]*$ ]] || { echo 'Invalid desktop user' >&2; exit 2; }
uid=$(id -u "$user")
[[ "$uid" != 0 ]] || { echo 'Select the desktop user, not root' >&2; exit 2; }
group=$(id -gn "$user")
home=$(getent passwd "$user" | cut -d: -f6)
root=$(cd "$(dirname "$0")/.." && pwd)
[[ -x "$root/target/release/afterburner-rs" ]] || { echo 'Build first: cargo build --release --offline' >&2; exit 1; }

systemctl stop afterburner-rs.service 2>/dev/null || true
systemctl stop afterburner-rs-fan.service 2>/dev/null || true
install -m 0755 "$root/target/release/afterburner-rs" /usr/local/bin/afterburner-rs.new
mv /usr/local/bin/afterburner-rs.new /usr/local/bin/afterburner-rs
sed -e "s/@GROUP@/$group/g" -e "s/@UID@/$uid/g" "$root/extra/afterburner-rs-fan.service.in" > /etc/systemd/system/afterburner-rs-fan.service

unit=$(< "$root/extra/afterburner-rs.service.in")
unit=${unit//@USER@/"$user"}
unit=${unit//@GROUP@/"$group"}
unit=${unit//@HOME@/"$home"}
printf '%s\n' "$unit" > /etc/systemd/system/afterburner-rs.service

install -Dm0644 "$root/extra/afterburner-rs-gui.desktop" /usr/share/applications/afterburner-rs-gui.desktop
install -Dm0644 "$root/src/assets/icon.svg" /usr/share/icons/hicolor/scalable/apps/afterburner-rs.svg
install -d -m 0755 -o "$user" -g "$group" "$home/.local/share/applications" "$home/.local/share/icons/hicolor/scalable/apps"
install -m 0644 -o "$user" -g "$group" "$root/extra/afterburner-rs-gui.desktop" "$home/.local/share/applications/afterburner-rs-gui.desktop"
install -m 0644 -o "$user" -g "$group" "$root/src/assets/icon.svg" "$home/.local/share/icons/hicolor/scalable/apps/afterburner-rs.svg"

# Legacy publication from the earlier root daemon: fix only its two known files.
for path in /dev/shm/MAHMSharedMemory /dev/shm/Access_MAHMSharedMemory; do
    if [[ -f "$path" && ! -L "$path" ]]; then chown --no-dereference "$user:$group" "$path"; fi
done
command -v update-desktop-database >/dev/null && update-desktop-database /usr/share/applications
command -v update-desktop-database >/dev/null && runuser -u "$user" -- update-desktop-database "$home/.local/share/applications"
systemctl daemon-reload
systemctl enable --now afterburner-rs-fan.service afterburner-rs.service
echo 'Afterburner RS GTK installed. Launch: afterburner-rs gui'
