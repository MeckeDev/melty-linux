# Fix for Mashup Startup Issues on Linux

## Problem
Some mashups like "Minecraft Ring" and "PeakCraft" did not start on Linux because the startup script only checked for `me3-launcher.exe`, but these mashups use `prismlauncher.exe`.

## Solution
Extended the `is_me3` check in `launch_exe()` (`src-tauri/src/mods/launch.rs`) by `prismlauncher.exe`:

```rust
// before:
let is_me3 = name.contains("me3-launcher.exe");

// after:
let is_me3 = name.contains("me3-launcher.exe") || name.contains("prismlauncher.exe");
```

## Why is this important?
The `me3-launcher.exe` and `prismlauncher.exe` are Minecraft launchers that:
1. Require the argument `--launch <slug>` (instead of the default arguments defined in the recipe)
2. Need the environment variable `ERBRIDGE=1` to activate the Minecraft integration via the bridge (`erbridge/core.dll`)

## Changes
- `src-tauri/src/mods/launch.rs`: extended `is_me3` check by `prismlauncher.exe`
- Added new test `prismlauncher_is_treated_like_me3_launcher`
- 72 tests run through
- Release binary successfully built

## Expected Behavior
- "Minecraft Ring" and "PeakCraft" should now start correctly
- The bridge (`erbridge/core.dll`) is activated
- Minecraft integration works as on Windows

## Testing
- `cargo test --workspace` → all 72 tests passed
- `cargo build --release` → successful
- E2E tests show that mashups are loaded correctly
