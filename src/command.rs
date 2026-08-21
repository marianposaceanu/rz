use std::{ffi::OsStr, process::Command};

use anyhow::{Context, Result, bail};

pub fn run(program: impl AsRef<OsStr>, args: &[impl AsRef<OsStr>]) -> Result<String> {
    let program = program.as_ref();
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {}", program.to_string_lossy()))?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        bail!("{} failed: {}", program.to_string_lossy(), combined.trim());
    }
    Ok(combined.trim().to_owned())
}

pub fn try_run(program: impl AsRef<OsStr>, args: &[impl AsRef<OsStr>]) -> Option<String> {
    run(program, args).ok()
}
