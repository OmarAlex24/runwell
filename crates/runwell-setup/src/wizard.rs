//! Human prompts never ask for secrets. Key enrollment happens in the user's terminal.
use crate::{
    Error,
    model::{HostTarget, RepoTarget, SetupSession},
    ssh::{SshClient, copy_key_command, local_public_key},
};
use dialoguer::{Confirm, Input};

pub(crate) fn targets(state: &SetupSession) -> Result<(Vec<HostTarget>, Vec<RepoTarget>), Error> {
    let mut hosts = Vec::new();
    let mut repos = Vec::new();
    if !state.hosts.is_empty() {
        eprintln!(
            "Resuming {} host(s) and {} repo(s).",
            state.hosts.len(),
            state.repos.len()
        );
        if Confirm::new()
            .with_prompt("Refresh the saved hosts?")
            .default(true)
            .interact()?
        {
            hosts.extend(state.hosts.iter().map(|host| host.target.clone()));
        }
    }
    if state.hosts.is_empty()
        || Confirm::new()
            .with_prompt("Add a host?")
            .default(false)
            .interact()?
    {
        loop {
            let host: String = Input::new()
                .with_prompt("SSH destination user@host[:port]")
                .validate_with(|input: &String| {
                    input
                        .parse::<HostTarget>()
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
                .interact_text()?;
            let host = host.parse()?;
            if !hosts.contains(&host) {
                hosts.push(host);
            }
            if !Confirm::new()
                .with_prompt("Add another host?")
                .default(false)
                .interact()?
            {
                break;
            }
        }
    }
    if state.repos.is_empty()
        || Confirm::new()
            .with_prompt("Add a repository?")
            .default(false)
            .interact()?
    {
        loop {
            let repo: String = Input::new()
                .with_prompt("Repository owner/name")
                .validate_with(|input: &String| {
                    input
                        .parse::<RepoTarget>()
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                })
                .interact_text()?;
            repos.push(repo.parse()?);
            if !Confirm::new()
                .with_prompt("Add another repository?")
                .default(false)
                .interact()?
            {
                break;
            }
        }
    }
    Ok((hosts, repos))
}

pub(crate) async fn authenticate(client: &SshClient, target: &HostTarget) -> Result<(), Error> {
    loop {
        match client.test_auth(target).await {
            Ok(()) => return Ok(()),
            Err(error @ (Error::Authentication(_) | Error::Timeout(_))) => {
                eprintln!("{error}");
                let home = dirs::home_dir();
                let key = home.as_deref().and_then(local_public_key);
                if let Some(key) = key {
                    eprintln!(
                        "Run this command in your own terminal:\n  {}",
                        copy_key_command(target, &key)
                    );
                } else {
                    eprintln!(
                        "No conventional public SSH key was found. To create one, run in your own terminal:\n  ssh-keygen -t ed25519"
                    );
                    if let Some(home) = home {
                        eprintln!(
                            "Then run:\n  {}",
                            copy_key_command(target, &home.join(".ssh/id_ed25519.pub"))
                        );
                    }
                }
                eprintln!("runwell only uses SSH keys and never requests a password.");
                if !Confirm::new()
                    .with_prompt("Retry after fixing SSH access in your terminal?")
                    .default(true)
                    .interact()?
                {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
}
