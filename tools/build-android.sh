#!/usr/bin/env bash
# Builds the Rust core for Android and the APK. Run from the repo root in Git Bash.
set -e
export PATH="$HOME/.cargo/bin:$PATH"
for t in aarch64-linux-android:arm64-v8a x86_64-linux-android:x86_64; do
  triple=${t%%:*}
  abi=${t##*:}
  cargo build --profile android -p hs-android --target "$triple"
  mkdir -p "shell-android/app/src/main/jniLibs/$abi"
  cp "target/$triple/android/libhs_android.so" "shell-android/app/src/main/jniLibs/$abi/"
done
cd shell-android
export JAVA_HOME="C:/Program Files/Eclipse Adoptium/jdk-17.0.19.10-hotspot"
export ANDROID_HOME="C:/Android"
export PATH="$JAVA_HOME/bin:$PATH"
env -u MSYS_NO_PATHCONV ./gradlew assembleRelease -q
ls -la app/build/outputs/apk/release/
