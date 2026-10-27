#!/usr/bin/env bash
set -euo pipefail

rewrite_ci_mirrors() {
  local apt_directory="$1" source_file
  for source_file in "$apt_directory/sources.list" "$apt_directory/sources.list.d/ubuntu.sources" \
    "$apt_directory/apt-mirrors.txt" "$apt_directory/apt-security-mirrors.txt"; do
    if [[ -f "$source_file" ]]; then
      sudo -n sed -i 's|http://azure\.archive\.ubuntu\.com/ubuntu|https://archive.ubuntu.com/ubuntu|g' "$source_file"
    fi
  done
}

install_ci_packages() {
  local apt_options=(-o Acquire::Retries=2 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 -o DPkg::Lock::Timeout=60)
  sudo -n env DEBIAN_FRONTEND=noninteractive apt-get "${apt_options[@]}" update
  sudo -n env DEBIAN_FRONTEND=noninteractive apt-get "${apt_options[@]}" install --no-install-recommends -y -- "$@"
}

main() {
  if [[ $# -eq 0 ]]; then
    printf '%s\n' 'Usage: install-ci-linux-deps.sh PACKAGE...' >&2
    return 64
  fi
  # Hosted images use both direct sources and mirror+file lists.
  rewrite_ci_mirrors /etc/apt
  install_ci_packages "$@"
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
