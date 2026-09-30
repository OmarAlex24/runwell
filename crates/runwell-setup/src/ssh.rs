//! System SSH transport. There is no password input API or SSH library.
use crate::{Error, model::HostTarget};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};

/// Immutable required options shared by authentication and probing.
pub fn ssh_args(target: &HostTarget, remote_command: &[&str]) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "StrictHostKeyChecking=accept-new",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    if let Some(port) = target.port() {
        args.extend(["-p".into(), port.to_string().into()]);
    }
    args.extend(["--".into(), target.destination().into()]);
    args.extend(remote_command.iter().map(OsString::from));
    args
}

/// Bounded transport; the production executable is always resolved as `ssh`.
#[derive(Debug, Clone)]
pub struct SshClient {
    executable: PathBuf,
    deadline: Duration,
}
impl Default for SshClient {
    fn default() -> Self {
        Self::new("ssh", Duration::from_secs(120))
    }
}
impl SshClient {
    /// An injectable executable and deadline for transport tests.
    pub fn new(executable: impl Into<PathBuf>, deadline: Duration) -> Self {
        Self {
            executable: executable.into(),
            deadline,
        }
    }

    pub async fn test_auth(&self, target: &HostTarget) -> Result<(), Error> {
        let status = self.invoke(target, &["true"], None).await?.status;
        if status.success() {
            Ok(())
        } else {
            Err(Error::Authentication(target.to_string()))
        }
    }

    pub async fn probe(&self, target: &HostTarget) -> Result<Vec<u8>, Error> {
        let output = self
            .invoke(target, &["sh", "-s"], Some(crate::probe::SCRIPT))
            .await?;
        if !output.status.success() {
            return Err(Error::Ssh(target.to_string()));
        }
        Ok(output.stdout)
    }

    async fn invoke(
        &self,
        target: &HostTarget,
        command: &[&str],
        input: Option<&str>,
    ) -> Result<std::process::Output, Error> {
        let operation = async {
            let mut child = Command::new(&self.executable)
                .args(ssh_args(target, command))
                .stdin(if input.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()?;
            // Feed concurrently with output collection so neither SSH pipe can
            // deadlock when a remote host closes stdin early or prints a banner.
            let stdin = child.stdin.take();
            let write = async {
                if let (Some(mut stdin), Some(input)) = (stdin, input) {
                    stdin.write_all(input.as_bytes()).await?;
                    stdin.shutdown().await?;
                }
                Ok::<(), std::io::Error>(())
            };
            let (written, output) = tokio::join!(write, child.wait_with_output());
            let output = output?;
            // Preserve the SSH failure classification if the pipe closed on auth failure.
            if output.status.success() {
                written?;
            }
            Ok::<_, Error>(output)
        };
        timeout(self.deadline, operation)
            .await
            .map_err(|_| Error::Timeout(target.to_string()))?
    }
}

/// Look for conventional public keys only; never read any private key contents.
pub fn local_public_key(home: &Path) -> Option<PathBuf> {
    [
        "id_ed25519.pub",
        "id_ecdsa.pub",
        "id_rsa.pub",
        "id_ed25519_sk.pub",
        "id_ecdsa_sk.pub",
    ]
    .iter()
    .map(|name| home.join(".ssh").join(name))
    .find(|path| path.is_file())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// A command for the user to run in their own terminal; runwell never executes it.
pub fn copy_key_command(target: &HostTarget, key: &Path) -> String {
    let port = target
        .port()
        .map(|port| format!(" -p {port}"))
        .unwrap_or_default();
    format!(
        "ssh-copy-id -i {}{port} {}",
        shell_quote(&key.to_string_lossy()),
        shell_quote(&target.destination())
    )
}
