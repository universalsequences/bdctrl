use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

use crate::model::DashboardData;

#[derive(Clone, Debug)]
pub struct BdClient {
    project: PathBuf,
}

impl BdClient {
    pub fn new(project: PathBuf) -> Self {
        Self { project }
    }
    pub fn project(&self) -> &Path {
        &self.project
    }

    pub fn load(&self) -> Result<DashboardData> {
        let output = self.run(["export"])?;
        DashboardData::from_export(&output)
    }

    pub fn set_priority(&self, id: &str, priority: u8) -> Result<()> {
        self.run(["priority", id, &priority.to_string()]).map(drop)
    }

    pub fn set_parent(&self, id: &str, parent: Option<&str>) -> Result<()> {
        self.run(["update", id, "--parent", parent.unwrap_or("")])
            .map(drop)
    }

    fn run<const N: usize>(&self, args: [&str; N]) -> Result<String> {
        let output = Command::new("bd")
            .args(args)
            .current_dir(&self.project)
            .output()
            .with_context(|| format!("could not run bd in {}", self.project.display()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            bail!(
                "{}",
                if stderr.is_empty() {
                    format!("bd exited {}", output.status)
                } else {
                    stderr
                }
            );
        }
        String::from_utf8(output.stdout).context("bd returned non-UTF-8 output")
    }
}
