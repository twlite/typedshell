use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const RELEASE_DOWNLOAD_BASE: &str = "https://github.com/twlite/typedshell/releases/latest/download";

pub fn upgrade() -> miette::Result<i32> {
    let executable = std::env::current_exe()
        .map_err(|error| miette::miette!("cannot locate the current tsh executable: {error}"))?;
    let parent = executable
        .parent()
        .ok_or_else(|| miette::miette!("the current executable has no parent directory"))?;
    let asset = release_asset(std::env::consts::OS, std::env::consts::ARCH)?;
    let download_url = format!("{RELEASE_DOWNLOAD_BASE}/{asset}");
    let staged = staged_path(parent)?;

    let download = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" })
        .args(["-fLsS", "--retry", "3", "-o"])
        .arg(&staged)
        .arg(&download_url)
        .status()
        .map_err(|error| {
            let _ = fs::remove_file(&staged);
            miette::miette!("cannot start curl to download the latest tsh release: {error}")
        })?;
    if !download.success() {
        let _ = fs::remove_file(&staged);
        return Err(miette::miette!(
            "downloading the latest tsh release failed with {download}"
        ));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755)).map_err(|error| {
            let _ = fs::remove_file(&staged);
            miette::miette!("cannot mark the downloaded executable as executable: {error}")
        })?;
    }

    let version = Command::new(&staged)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| {
            let _ = fs::remove_file(&staged);
            miette::miette!("the downloaded tsh executable could not be started: {error}")
        })?;
    if !version.status.success() {
        let _ = fs::remove_file(&staged);
        return Err(miette::miette!(
            "the downloaded file is not a working tsh executable"
        ));
    }
    let version_line = String::from_utf8_lossy(&version.stdout);
    eprintln!("Downloaded {}", version_line.trim());

    #[cfg(unix)]
    {
        fs::rename(&staged, &executable).map_err(|error| {
            let _ = fs::remove_file(&staged);
            miette::miette!(
                "cannot replace {} with the latest tsh executable: {error}",
                executable.display()
            )
        })?;
        eprintln!("Updated {}", executable.display());
    }

    #[cfg(windows)]
    schedule_windows_replacement(&executable, &staged)?;

    Ok(0)
}

fn release_asset(os: &str, arch: &str) -> miette::Result<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Ok("typedshell-linux-x86_64"),
        ("linux", "aarch64") => Ok("typedshell-linux-aarch64"),
        ("macos", "x86_64") => Ok("typedshell-macos-x86_64"),
        ("macos", "aarch64") => Ok("typedshell-macos-aarch64"),
        ("windows", "x86_64") => Ok("typedshell-windows-x86_64.exe"),
        _ => Err(miette::miette!(
            "self-upgrade is not available for {os}/{arch}; download a release from https://github.com/twlite/typedshell/releases"
        )),
    }
}

fn staged_path(parent: &Path) -> miette::Result<PathBuf> {
    let suffix = if cfg!(windows) { ".exe" } else { ".bin" };
    let temporary = tempfile::Builder::new()
        .prefix(".tsh-upgrade-")
        .suffix(suffix)
        .tempfile_in(parent)
        .map_err(|error| {
            miette::miette!(
                "cannot create a temporary file beside {}: {error}",
                parent.display()
            )
        })?;
    let (file, path) = temporary.keep().map_err(|error| {
        miette::miette!(
            "cannot prepare the temporary download file: {}",
            error.error
        )
    })?;
    drop(file);
    Ok(path)
}

#[cfg(windows)]
fn schedule_windows_replacement(executable: &Path, staged: &Path) -> miette::Result<()> {
    let temporary = tempfile::Builder::new()
        .prefix("tsh-upgrade-")
        .suffix(".ps1")
        .tempfile()
        .map_err(|error| miette::miette!("cannot create the Windows upgrade helper: {error}"))?;
    let (file, helper) = temporary.keep().map_err(|error| {
        miette::miette!("cannot prepare the Windows upgrade helper: {}", error.error)
    })?;
    drop(file);
    fs::write(
        &helper,
        r#"param([string]$TargetPath, [string]$StagedPath, [int]$ProcessId)
while (Get-Process -Id $ProcessId -ErrorAction SilentlyContinue) {
    Start-Sleep -Milliseconds 100
}
Move-Item -LiteralPath $StagedPath -Destination $TargetPath -Force
Remove-Item -LiteralPath $MyInvocation.MyCommand.Path -Force -ErrorAction SilentlyContinue
"#,
    )
    .map_err(|error| miette::miette!("cannot write the Windows upgrade helper: {error}"))?;

    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-File",
        ])
        .arg(&helper)
        .arg(executable)
        .arg(staged)
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            miette::miette!("cannot schedule the Windows executable replacement: {error}")
        })?;

    eprintln!(
        "Upgrade downloaded. The new version will replace {} after this process exits.",
        executable.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::release_asset;

    #[test]
    fn maps_supported_release_platforms() {
        assert_eq!(
            release_asset("linux", "x86_64").unwrap(),
            "typedshell-linux-x86_64"
        );
        assert_eq!(
            release_asset("macos", "aarch64").unwrap(),
            "typedshell-macos-aarch64"
        );
        assert_eq!(
            release_asset("windows", "x86_64").unwrap(),
            "typedshell-windows-x86_64.exe"
        );
        assert!(release_asset("freebsd", "x86_64").is_err());
    }
}
