use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

const PLUGIN_NAME: &str = "cst-lifecycle";
const PLUGIN_MANIFEST: &str = include_str!("../copilot-plugin/plugin.json");
const HOOKS_TEMPLATE: &str = include_str!("../copilot-plugin/hooks.json");
const RECEIPT_FILE: &str = ".cst-managed.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginStatus {
    Installed,
    NotInstalled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshOutcome {
    NotManaged,
    Current,
    Refreshed,
    Removed,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
struct ManagedPluginReceipt {
    cst_version: String,
    executable: PathBuf,
    bundle_sha256: String,
}

pub fn install(copilot_home: &Path) -> Result<()> {
    install_with(copilot_home, |plugin| {
        register(copilot_home, plugin, "install the CST lifecycle plugin")
    })
}

pub fn uninstall(copilot_home: &Path) -> Result<()> {
    let output = copilot_command(copilot_home)
        .args(["plugin", "uninstall", PLUGIN_NAME])
        .output()
        .context("Failed to start `copilot plugin uninstall`")?;
    command_result(&output, "uninstall the CST lifecycle plugin")?;
    let root = plugin_root(copilot_home);
    match std::fs::remove_dir_all(&root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("Failed to remove lifecycle hooks at {}", root.display())
            });
        }
    }
    Ok(())
}

pub fn status(copilot_home: &Path) -> Result<PluginStatus> {
    let output = copilot_command(copilot_home)
        .args(["plugin", "list"])
        .output()
        .context("Failed to start `copilot plugin list`")?;
    if !output.status.success() {
        command_result(&output, "list Copilot plugins")?;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(if stdout.contains(PLUGIN_NAME) {
        PluginStatus::Installed
    } else {
        PluginStatus::NotInstalled
    })
}

pub fn refresh_if_needed(copilot_home: &Path) -> Result<RefreshOutcome> {
    refresh_if_needed_with(
        copilot_home,
        || Ok(status(copilot_home)? == PluginStatus::Installed),
        |plugin| register(copilot_home, plugin, "refresh the CST lifecycle plugin"),
    )
}

fn refresh_if_needed_with<S, I>(
    copilot_home: &Path,
    installed: S,
    install_plugin: I,
) -> Result<RefreshOutcome>
where
    S: FnOnce() -> Result<bool>,
    I: FnOnce(&Path) -> Result<()>,
{
    let root = plugin_root(copilot_home);
    if !root.is_dir() {
        return Ok(RefreshOutcome::NotManaged);
    }

    let desired = desired_receipt()?;
    if read_receipt(&root)? == Some(desired) {
        return Ok(RefreshOutcome::Current);
    }

    if !installed()? {
        std::fs::remove_dir_all(&root).with_context(|| {
            format!(
                "Failed to remove stale lifecycle hooks at {}",
                root.display()
            )
        })?;
        return Ok(RefreshOutcome::Removed);
    }

    install_with(copilot_home, install_plugin)?;
    Ok(RefreshOutcome::Refreshed)
}

fn copilot_command(copilot_home: &Path) -> Command {
    let mut command = Command::new("copilot");
    command.env("COPILOT_HOME", copilot_home);
    command
}

fn register(copilot_home: &Path, plugin: &Path, action: &str) -> Result<()> {
    let output = copilot_command(copilot_home)
        .args(["plugin", "install"])
        .arg(plugin)
        .output()
        .context("Failed to start `copilot plugin install`")?;
    command_result(&output, action)
}

fn command_result(output: &std::process::Output, action: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    anyhow::bail!(
        "Could not {action}{}",
        if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    )
}

fn materialize(copilot_home: &Path) -> Result<PathBuf> {
    let root = plugin_root(copilot_home);
    std::fs::create_dir_all(&root)
        .with_context(|| format!("Failed to create {}", root.display()))?;
    std::fs::write(root.join("plugin.json"), PLUGIN_MANIFEST)
        .context("Failed to write CST plugin manifest")?;

    let executable = crate::updater::invocation_executable()?;
    let mut hooks: Value =
        serde_json::from_str(HOOKS_TEMPLATE).context("Bundled CST hooks are invalid")?;
    let events = hooks
        .get_mut("hooks")
        .and_then(Value::as_object_mut)
        .context("Bundled CST hooks have no hooks object")?;
    for entries in events.values_mut().filter_map(Value::as_array_mut) {
        for entry in entries.iter_mut().filter_map(Value::as_object_mut) {
            let event = entry
                .remove("command")
                .and_then(|value| value.as_str().map(str::to_string))
                .and_then(|command| command.split_whitespace().last().map(str::to_string))
                .context("Bundled CST hook command is invalid")?;
            entry.insert(
                "bash".to_string(),
                Value::String(format!(
                    "{} hook-event {event}",
                    quote_bash(&executable.to_string_lossy())
                )),
            );
            entry.insert(
                "powershell".to_string(),
                Value::String(format!(
                    "& {} hook-event {event}",
                    quote_powershell(&executable.to_string_lossy())
                )),
            );
        }
    }
    std::fs::write(root.join("hooks.json"), serde_json::to_vec_pretty(&hooks)?)
        .context("Failed to write CST plugin hooks")?;
    Ok(root)
}

fn install_with<F>(copilot_home: &Path, install_plugin: F) -> Result<()>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let plugin = materialize(copilot_home)?;
    install_plugin(&plugin)?;
    write_receipt(&plugin, &desired_receipt()?)
}

fn desired_receipt() -> Result<ManagedPluginReceipt> {
    let executable = crate::updater::invocation_executable()?;
    let mut digest = Sha256::new();
    digest.update(PLUGIN_MANIFEST.as_bytes());
    digest.update([0]);
    digest.update(HOOKS_TEMPLATE.as_bytes());
    Ok(ManagedPluginReceipt {
        cst_version: env!("CARGO_PKG_VERSION").to_string(),
        executable,
        bundle_sha256: format!("{:x}", digest.finalize()),
    })
}

fn read_receipt(root: &Path) -> Result<Option<ManagedPluginReceipt>> {
    let path = root.join(RECEIPT_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to read {}", path.display()));
        }
    };
    // An interrupted older install is treated as stale and repaired after checking that
    // the plugin is still registered with Copilot.
    Ok(serde_json::from_slice(&bytes).ok())
}

fn write_receipt(root: &Path, receipt: &ManagedPluginReceipt) -> Result<()> {
    let path = root.join(RECEIPT_FILE);
    std::fs::write(&path, serde_json::to_vec_pretty(receipt)?)
        .with_context(|| format!("Failed to write {}", path.display()))
}

fn plugin_root(copilot_home: &Path) -> PathBuf {
    copilot_home.join("cst").join("plugins").join(PLUGIN_NAME)
}

fn quote_bash(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn quote_powershell(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn materialized_hooks_use_the_exact_cst_executable_and_no_generic_command() {
        let temp = tempfile::tempdir().unwrap();
        let root = materialize(temp.path()).unwrap();
        let hooks: Value =
            serde_json::from_slice(&std::fs::read(root.join("hooks.json")).unwrap()).unwrap();
        let entries = hooks["hooks"].as_object().unwrap();
        assert_eq!(entries.len(), 6);
        for entry in entries
            .values()
            .flat_map(|entries| entries.as_array().unwrap())
        {
            assert!(entry.get("command").is_none());
            assert!(entry["bash"].as_str().unwrap().contains("hook-event"));
            assert!(entry["powershell"].as_str().unwrap().contains("hook-event"));
        }
    }

    #[test]
    fn shell_quoting_handles_spaces_and_apostrophes() {
        assert_eq!(
            quote_powershell(r"C:\Program Files\O'Brien\cst.exe"),
            r#"'C:\Program Files\O''Brien\cst.exe'"#
        );
        assert_eq!(quote_bash("/tmp/O'Brien/cst"), r#"'/tmp/O'"'"'Brien/cst'"#);
    }

    #[test]
    fn unmanaged_users_never_probe_or_install_the_plugin() {
        let temp = tempfile::tempdir().unwrap();
        let outcome = refresh_if_needed_with(
            temp.path(),
            || panic!("unmanaged hooks must not query Copilot"),
            |_| panic!("unmanaged hooks must not be installed"),
        )
        .unwrap();

        assert_eq!(outcome, RefreshOutcome::NotManaged);
    }

    #[test]
    fn current_receipt_skips_copilot_processes() {
        let temp = tempfile::tempdir().unwrap();
        install_with(temp.path(), |_| Ok(())).unwrap();

        let outcome = refresh_if_needed_with(
            temp.path(),
            || panic!("current hooks must not query Copilot"),
            |_| panic!("current hooks must not be reinstalled"),
        )
        .unwrap();

        assert_eq!(outcome, RefreshOutcome::Current);
    }

    #[test]
    fn legacy_install_is_refreshed_and_receipted_once() {
        let temp = tempfile::tempdir().unwrap();
        materialize(temp.path()).unwrap();
        let installs = Cell::new(0);

        let outcome = refresh_if_needed_with(
            temp.path(),
            || Ok(true),
            |_| {
                installs.set(installs.get() + 1);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(outcome, RefreshOutcome::Refreshed);
        assert_eq!(installs.get(), 1);
        assert_eq!(
            read_receipt(&plugin_root(temp.path())).unwrap(),
            Some(desired_receipt().unwrap())
        );
    }

    #[test]
    fn manual_copilot_uninstall_is_respected() {
        let temp = tempfile::tempdir().unwrap();
        let root = materialize(temp.path()).unwrap();
        write_receipt(
            &root,
            &ManagedPluginReceipt {
                cst_version: "0.0.0".to_string(),
                executable: PathBuf::from("old-cst"),
                bundle_sha256: "old-hooks".to_string(),
            },
        )
        .unwrap();

        let outcome = refresh_if_needed_with(
            temp.path(),
            || Ok(false),
            |_| panic!("a manually removed plugin must not be reinstalled"),
        )
        .unwrap();

        assert_eq!(outcome, RefreshOutcome::Removed);
        assert!(!root.exists());
    }

    #[test]
    fn failed_refresh_keeps_the_stale_receipt_for_a_retry() {
        let temp = tempfile::tempdir().unwrap();
        let root = materialize(temp.path()).unwrap();
        let old = ManagedPluginReceipt {
            cst_version: "0.0.0".to_string(),
            executable: PathBuf::from("old-cst"),
            bundle_sha256: "old-hooks".to_string(),
        };
        write_receipt(&root, &old).unwrap();

        let error = refresh_if_needed_with(
            temp.path(),
            || Ok(true),
            |_| anyhow::bail!("injected refresh failure"),
        )
        .unwrap_err();

        assert!(error.to_string().contains("injected refresh failure"));
        assert_eq!(read_receipt(&root).unwrap(), Some(old));
    }
}
