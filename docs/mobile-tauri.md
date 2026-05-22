# Tauri Mobile Build and Test Notes

This repo now carries the generated Android project under `apps/desktop/src-tauri/gen/android`. The app is still desktop-first in product scope, but the Android build path is validated from this Windows workspace.

## Official Entry Points

- Mobile prerequisites: https://v2.tauri.app/start/prerequisites/
- Mobile development flow: https://v2.tauri.app/develop/
- CLI commands: https://v2.tauri.app/reference/cli/
- WebDriver testing: https://v2.tauri.app/develop/tests/webdriver/

## Project Scripts

All commands run from the repository root:

```powershell
pnpm --dir apps/desktop tauri:android:init
pnpm --dir apps/desktop tauri:android:dev
pnpm --dir apps/desktop tauri:android:build
pnpm --dir apps/desktop tauri:android:run

pnpm --dir apps/desktop android:e-drive:check
pnpm --dir apps/desktop android:e-drive:check-typst
pnpm --dir apps/desktop android:e-drive:build-debug
pnpm --dir apps/desktop android:e-drive:build-debug-typst
pnpm --dir apps/desktop android:e-drive:build-release
pnpm --dir apps/desktop android:e-drive:build-release-typst
pnpm --dir apps/desktop android:e-drive:dev
pnpm --dir apps/desktop android:e-drive:dev-typst
pnpm --dir apps/desktop android:e-drive:devices

pnpm --dir apps/desktop tauri:ios:init
pnpm --dir apps/desktop tauri:ios:dev
pnpm --dir apps/desktop tauri:ios:build
```

The desktop package now carries `@tauri-apps/cli`, so these scripts do not depend on a globally installed `tauri` binary.

The `android:e-drive:*` scripts are Windows helpers for this workstation. They set:

- `ANDROID_HOME=E:\AndroidSDK`
- `ANDROID_SDK_ROOT=E:\AndroidSDK`
- `ANDROID_NDK_HOME=E:\AndroidSDK\ndk\28.0.12433566`
- `NDK_HOME=E:\AndroidSDK\ndk\28.0.12433566`
- PATH entries for the NDK LLVM toolchain, platform tools, and command-line tools.

Use the generic `tauri:android:*` scripts on machines where Android Studio already exports the toolchain through the normal environment.

## Android Render Profiles

Android has two render profiles:

- Default mobile profile: does not bundle native Typst. The app uses Markdown rendering and direct image preview by default on mobile, while keeping the same attachment, sync, and storage paths.
- Experimental Typst profile: enables `mobile-typst-render`, bundling Typst, embedded fonts, and native SVG rendering into the Android binary.

Use the default mobile profile for normal Android iteration:

```powershell
pnpm --dir apps/desktop android:e-drive:check
pnpm --dir apps/desktop android:e-drive:build-release
```

Use the experimental native Typst profile only when specifically testing Typst-on-Android:

```powershell
pnpm --dir apps/desktop android:e-drive:check-typst
pnpm --dir apps/desktop android:e-drive:build-release-typst
```

The dependency split is implemented by keeping `memo-render`'s `typst-render` feature enabled by default for desktop and tests, while Android depends on the lightweight renderer unless `mobile-typst-render` is explicitly activated.

## Toolchain Requirements

Android:

- Android Studio with SDK, NDK, platform tools, and an emulator or physical device.
- Rust mobile targets installed through the Tauri prerequisite flow.
- On this Windows machine, the validated SDK root is `E:\AndroidSDK`, with NDK `28.0.12433566`.
- On Windows, ensure the NDK toolchain is on PATH or configured through Android Studio. A library check for `aarch64-linux-android` needs `aarch64-linux-android-clang` and the Android sysroot headers. The `android:e-drive:*` scripts handle this locally.
- For emulator testing, do not point sync to `127.0.0.1` unless the server is inside the emulator. Use `10.0.2.2:<port>` for the Android emulator host bridge, or the host LAN address for physical devices.

iOS:

- macOS with Xcode, the iOS Rust target, and CocoaPods where required by the generated project.
- iOS work cannot be validated from this Windows workspace; the local Tauri CLI exposes Android commands here, while iOS commands should be run on a macOS runner or developer machine.

## Development Server

Tauri mobile can pass a development host through `TAURI_DEV_HOST`. `apps/desktop/vite.config.ts` reads that variable and falls back to `127.0.0.1`.

Recommended local pattern:

```powershell
$env:TAURI_DEV_HOST="0.0.0.0"
pnpm --dir apps/desktop tauri:android:dev
```

The E-drive helper already sets `TAURI_DEV_HOST=0.0.0.0` for `android:e-drive:dev`.

For device testing, ensure the phone and development machine are on the same network, the firewall allows the Vite port, and sync server URLs use an address visible to the device.

## Current App Caveats

- Android uses `apps/desktop/src-tauri/tauri.android.conf.json`, with a single `main` webview and the `app.memosync.mobile` identifier.
- Desktop-only tray, global shortcuts, single-instance behavior, always-on-top quick capture, and desktop-style window controls are feature-gated out of mobile builds.
- Quick capture and settings are handled as in-app mobile surfaces instead of separate OS windows.
- Large attachments and pure image notes stay on the binary resource path instead of being forced through the Typst render path.
- Mobile `auto` preview resolves to Markdown/direct-image preview. Manual Typst preview is still available when the Android build is made with `mobile-typst-render`.
- Mobile webviews are stricter about memory pressure. Prefer cache caps, streaming or chunked sync payloads, and explicit cleanup of generated previews.
- The generated Gradle project disables Kotlin incremental compilation and uses in-process Kotlin compilation. This avoids Windows multi-drive path-cache failures when Cargo registry sources live on `E:` and the repo lives on `F:`.

## Test Plan

Baseline checks before native mobile work:

```powershell
pnpm --dir apps/desktop test
pnpm --dir apps/desktop build
git diff --check
```

Optional Android library check after the NDK is installed:

```powershell
cargo check -p memo-desktop --lib --target aarch64-linux-android
```

Validated local Android commands:

```powershell
pnpm --dir apps/desktop android:e-drive:check
pnpm --dir apps/desktop android:e-drive:check-typst
pnpm --dir apps/desktop android:e-drive:build-debug
pnpm --dir apps/desktop android:e-drive:build-release
pnpm --dir apps/desktop android:e-drive:devices
```

Current validated APK outputs:

- Debug: `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk`
- Release unsigned: `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk`

Observed size notes:

- Default release unsigned APK is about 13.8 MB; `target/aarch64-linux-android/release/libmemo_desktop_lib.so` is about 11.1 MB.
- Experimental Typst release unsigned APK was about 118 MB; `target/aarch64-linux-android/release/libmemo_desktop_lib.so` was about 116 MB because it bundled Typst, image/font handling, embedded Chinese/code fonts, SQLite, HTTP, and the Tauri runtime.
- Debug APKs are much larger because Rust debug dynamic libraries include debug symbols.
- `cargo tree -p memo-desktop --target aarch64-linux-android -i typst` and `-i typst-as-lib` should print nothing for the default mobile profile.

Android smoke test:

1. Start the sync server on a host-visible address.
2. Run `pnpm --dir apps/desktop tauri:android:init` only when regenerating the Android project.
3. Run `pnpm --dir apps/desktop android:e-drive:dev` on an emulator and a physical device.
4. Verify creating, editing, tagging, searching, previewing Markdown, previewing Typst, attaching images, pure-image preview, and sync retry behavior.
5. Build with `pnpm --dir apps/desktop android:e-drive:build-release` and repeat the core smoke flow on the release artifact.

The latest `adb devices` check completed successfully but no emulator or physical device was attached, so install/run validation is still pending.

iOS smoke test:

1. Run the same flow on macOS with `tauri:ios:init`, `tauri:ios:dev`, and `tauri:ios:build`.
2. Verify safe-area layout, keyboard resize behavior, scroll containment, and sync URLs on simulator and device.

Automated E2E direction:

- Keep fast behavior tests in Vitest and Rust unit/integration tests.
- Use Tauri's WebDriver guidance for native shell automation; on mobile, plan for Appium 2 plus platform drivers.
- Start with a small E2E suite: app launch, create memo, attach one small image, render preview, restart, and verify local persistence.
- Add sync E2E only after the mobile URL mapping is stable, because emulator/device host addressing is the most common false failure.
