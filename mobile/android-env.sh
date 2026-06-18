# Android build environment for this macOS machine (Apple Silicon).
# Source before any ubrn/gradle/adb command:  `source mobile/android-env.sh`
#
# Set up 2026-06-18 alongside a verified `gradlew assembleDebug` (Expo SDK 55 /
# RN 0.83.6). iOS native builds are NOT possible on this machine — Expo SDK 55
# needs Xcode 26 / macOS 26 and this is macOS 14.7 + Xcode 15.1 (see mobile/CLAUDE.md).
export JAVA_HOME="$(brew --prefix openjdk@17)/libexec/openjdk.jdk/Contents/Home"
export ANDROID_HOME="$HOME/Library/Android/sdk"
export ANDROID_SDK_ROOT="$ANDROID_HOME"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.1.12297006"
export ANDROID_NDK_ROOT="$ANDROID_NDK_HOME"
export PATH="$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$PATH"
