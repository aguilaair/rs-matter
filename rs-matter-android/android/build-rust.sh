#!/usr/bin/env bash
# Builds librs_matter_android.so for the given Android ABIs into matter/src/main/jniLibs/<abi>/.
#
#   ./build-rust.sh [armeabi-v7a] [arm64-v8a] [x86_64] [x86]
#
# Needs: rustup with the matching targets (rustup target add armv7-linux-androideabi ...), and the
# Android NDK (ANDROID_NDK_HOME, or the newest one under $ANDROID_HOME/ndk or ~/Library/Android/sdk/ndk).
# PROFILE=debug builds without optimisations; API=26 is the minimum API level linked against.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
crate="$here/.."
out="$here/matter/src/main/jniLibs"
profile="${PROFILE:-release}"
api="${API:-26}"

ndk="${ANDROID_NDK_HOME:-}"
if [ -z "$ndk" ]; then
  sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}"
  ndk="$(ls -d "$sdk"/ndk/* 2>/dev/null | sort -V | tail -1 || true)"
fi
[ -d "$ndk" ] || { echo "Android NDK not found: set ANDROID_NDK_HOME" >&2; exit 1; }
bin="$(ls -d "$ndk"/toolchains/llvm/prebuilt/*/bin | head -1)"

[ $# -gt 0 ] || set -- armeabi-v7a arm64-v8a x86_64 x86

for abi in "$@"; do
  case "$abi" in
    armeabi-v7a) target=armv7-linux-androideabi; clang=armv7a-linux-androideabi$api-clang ;;
    arm64-v8a)   target=aarch64-linux-android;   clang=aarch64-linux-android$api-clang ;;
    x86_64)      target=x86_64-linux-android;    clang=x86_64-linux-android$api-clang ;;
    x86)         target=i686-linux-android;      clang=i686-linux-android$api-clang ;;
    *) echo "unknown ABI: $abi" >&2; exit 1 ;;
  esac
  t="$(echo "$target" | tr '-' '_')"
  T="$(echo "$t" | tr '[:lower:]' '[:upper:]')"
  export "CC_$t=$bin/$clang" "AR_$t=$bin/llvm-ar" "CARGO_TARGET_${T}_LINKER=$bin/$clang"
  flags=(--target "$target" -p rs-matter-android)
  [ "$profile" = release ] && flags+=(--release)
  (cd "$crate" && cargo build "${flags[@]}")
  target_dir="${CARGO_TARGET_DIR:-$crate/../target}"
  mkdir -p "$out/$abi"
  cp "$target_dir/$target/$profile/librs_matter_android.so" "$out/$abi/"
  "$bin/llvm-strip" --strip-unneeded "$out/$abi/librs_matter_android.so"
  echo "$abi: $(du -h "$out/$abi/librs_matter_android.so" | cut -f1)"
done
