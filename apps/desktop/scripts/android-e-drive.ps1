param(
  [ValidateSet("check", "check-typst", "build-debug", "build-debug-typst", "build-release", "build-release-typst", "dev", "dev-typst", "run", "devices", "stop-gradle")]
  [string] $Task = "check",
  [string] $SdkRoot = "E:\AndroidSDK",
  [string] $NdkVersion = "28.0.12433566"
)

$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$desktopDir = Resolve-Path (Join-Path $scriptDir "..")
$repoRoot = Resolve-Path (Join-Path $desktopDir "..\..")
$ndkRoot = Join-Path $SdkRoot "ndk\$NdkVersion"
$ndkBin = Join-Path $ndkRoot "toolchains\llvm\prebuilt\windows-x86_64\bin"
$platformTools = Join-Path $SdkRoot "platform-tools"
$cmdlineTools = Join-Path $SdkRoot "cmdline-tools\latest\bin"

foreach ($path in @($SdkRoot, $ndkRoot, $ndkBin)) {
  if (-not (Test-Path $path)) {
    throw "Android toolchain path was not found: $path"
  }
}

$env:ANDROID_HOME = $SdkRoot
$env:ANDROID_SDK_ROOT = $SdkRoot
$env:ANDROID_NDK_HOME = $ndkRoot
$env:NDK_HOME = $ndkRoot
$env:PATH = "$ndkBin;$platformTools;$cmdlineTools;$env:PATH"

switch ($Task) {
  "check" {
    & cargo check -p memo-desktop --lib --target aarch64-linux-android
  }
  "check-typst" {
    & cargo check -p memo-desktop --lib --target aarch64-linux-android --features mobile-typst-render
  }
  "build-debug" {
    & pnpm --dir $desktopDir exec tauri android build --debug --target aarch64 --apk --ci
  }
  "build-debug-typst" {
    & pnpm --dir $desktopDir exec tauri android build --debug --target aarch64 --apk --features mobile-typst-render --ci
  }
  "build-release" {
    & pnpm --dir $desktopDir exec tauri android build --target aarch64 --apk --ci
  }
  "build-release-typst" {
    & pnpm --dir $desktopDir exec tauri android build --target aarch64 --apk --features mobile-typst-render --ci
  }
  "dev" {
    $env:TAURI_DEV_HOST = "0.0.0.0"
    & pnpm --dir $desktopDir exec tauri android dev
  }
  "dev-typst" {
    $env:TAURI_DEV_HOST = "0.0.0.0"
    & pnpm --dir $desktopDir exec tauri android dev --features mobile-typst-render
  }
  "run" {
    & pnpm --dir $desktopDir exec tauri android run --target aarch64
  }
  "devices" {
    & adb devices
  }
  "stop-gradle" {
    $androidDir = Join-Path $desktopDir "src-tauri\gen\android"
    & (Join-Path $androidDir "gradlew.bat") --stop -p $androidDir
  }
}

if ($LASTEXITCODE -ne 0) {
  exit $LASTEXITCODE
}
