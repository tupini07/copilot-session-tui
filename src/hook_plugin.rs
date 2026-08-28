use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const PLUGIN_NAME: &str = "cst-lifecycle";
const PLUGIN_MANIFEST: &str = include_str!("../copilot-plugin/plugin.json");
const HOOKS_TEMPLATE: &str = include_str!("../copilot-plugin/hooks.json");

pub enum PluginStatus {
    Installed,
    NotInstalled,
}

pub fn install(copilot_home: &Path) -> Result<()> {
    let plugin = materialize(copilot_home)?;
    let output = copilot_command(copilot_home)
        .args(["plugin", "install"])
        .arg(&plugin)
        .output()
        .context("Failed to start `copilot plugin install`")?;
    command_result(&output, "install the CST lifecycle plugin")
}

pub fn uninstall(copilot_home: &Path) -> Result<()> {
    let output = copilot_command(copilot_home)
        .args(["plugin", "uninstall", PLUGIN_NAME])
        .output()
        .context("Failed to start `copilot plugin uninstall`")?;
    command_result(&output, "uninstall the CST lifecycle plugin")?;
    let _ = std::fs::remove_dir_all(plugin_root(copilot_home));
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

fn copilot_command(copilot_home: &Path) -> Command {
    let mut command = Command::new("copilot");
    command.env("COPILOT_HOME", copilot_home);
    command
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
}
