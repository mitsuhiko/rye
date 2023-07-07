#!/usr/bin/env bash
set -euo pipefail

# Wrap everything in a function so that a truncated script
# does not have the chance to cause issues.
__wrap__() {

# allow overriding the version
VERSION=${RYE_VERSION:-latest}
# allow overriding the install option
INSTALL_OPTION=${RYE_INSTALL_OPTION:-""}

REPO=mitsuhiko/rye
PLATFORM=`uname -s`
ARCH=`uname -m`

if [[ $PLATFORM == "Darwin" ]]; then
  PLATFORM="macos"
elif [[ $PLATFORM == "Linux" ]]; then
  PLATFORM="linux"
fi

if [[ $ARCH == armv8* ]] || [[ $ARCH == arm64* ]] || [[ $ARCH == aarch64* ]]; then
  ARCH="aarch64"
elif [[ $ARCH == i686* ]]; then
  ARCH="x86"
fi

BINARY="rye-${ARCH}-${PLATFORM}"

# Oddly enough GitHub has different URLs for latest vs specific version
if [[ $VERSION == "latest" ]]; then
  DOWNLOAD_URL=https://github.com/${REPO}/releases/latest/download/${BINARY}.gz
else
  DOWNLOAD_URL=https://github.com/${REPO}/releases/download/${VERSION}/${BINARY}.gz
fi

echo "This script will automatically download and install rye (${VERSION}) for you."
if [ "x$(id -u)" == "x0" ]; then
  echo "warning: this script is running as root.  This is dangerous and unnecessary!"
fi

if ! hash curl 2> /dev/null; then
  echo "error: you do not have 'curl' installed which is required for this script."
  exit 1
fi

if ! hash gunzip 2> /dev/null; then
  echo "error: you do not have 'gunzip' installed which is required for this script."
  exit 1
fi

TEMP_FILE=`mktemp "${TMPDIR:-/tmp}/.ryeinstall.XXXXXXXX"`
TEMP_FILE_GZ="${TEMP_FILE}.gz"

cleanup() {
  rm -f "$TEMP_FILE"
  rm -f "$TEMP_FILE_GZ"
}

trap cleanup EXIT
HTTP_CODE=$(curl -SL --progress-bar "$DOWNLOAD_URL" --output "$TEMP_FILE_GZ" --write-out "%{http_code}")
if [[ ${HTTP_CODE} -lt 200 || ${HTTP_CODE} -gt 299 ]]; then
  echo "error: platform ${PLATFORM} (${ARCH}) is unsupported."
  exit 1
fi

rm -f "$TEMP_FILE"
gunzip "$TEMP_FILE_GZ" 
chmod +x "$TEMP_FILE"
"$TEMP_FILE" self install $INSTALL_OPTION

}; __wrap__
