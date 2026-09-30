use super::*;

impl Config {
    /// Parse TOML and validate its values without reading credential files.
    pub fn from_toml(source: &str) -> Result<Self, Error> {
        let config: Self = toml::from_str(source).map_err(|_| Error::Parse)?;
        config.validate()?;
        Ok(config)
    }

    /// Reject invalid reservations, ambiguous classes, paths, and PSI thresholds.
    pub fn validate(&self) -> Result<(), Error> {
        require(self.schema_version == 1, "schema_version must be 1")?;
        require(!self.node.id.trim().is_empty(), "node.id must not be empty")?;
        require(self.node.cpu_slots > 0, "node.cpu_slots must be positive")?;
        require(
            self.node.memory_bytes > 0,
            "node.memory_bytes must be positive",
        )?;
        let psi = &self.node.psi;
        require(
            psi.resume_percent.is_finite()
                && psi.pause_percent.is_finite()
                && psi.resume_percent >= 0.0
                && psi.resume_percent < psi.pause_percent
                && psi.pause_percent <= 100.0,
            "PSI thresholds must satisfy 0 <= resume_percent < pause_percent <= 100",
        )?;
        require(
            self.github.config_url.starts_with("https://")
                && self.github.config_url.len() > "https://".len(),
            "github.config_url must be a nonempty HTTPS URL",
        )?;
        require(
            !self.controller.classes.is_empty(),
            "controller.classes must not be empty",
        )?;
        let mut names = HashSet::new();
        for class in &self.controller.classes {
            require(
                !class.name.is_empty()
                    && class
                        .name
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-')
                    && names.insert(&class.name),
                "class names must be unique, nonempty, and contain only letters, digits, or hyphens",
            )?;
            require(
                class.cpu_slots > 0 && class.cpu_slots <= self.node.cpu_slots,
                "class CPU reservation must be positive and fit the node budget",
            )?;
            require(
                class.memory_high_bytes > 0
                    && class.memory_high_bytes <= class.memory_max_bytes
                    && class.memory_max_bytes <= self.node.memory_bytes,
                "class memory must satisfy 0 < memory_high_bytes <= memory_max_bytes <= node budget",
            )?;
            require(
                (1..=10000).contains(&class.cpu_weight),
                "cpu_weight must be between 1 and 10000",
            )?;
        }
        for path in [&self.controller.database, &self.node.state_dir] {
            require(path.is_absolute(), "state and TLS paths must be absolute")?;
        }
        if let Some(transport) = &self.transport {
            for path in [
                &transport.ca_file,
                &transport.certificate_file,
                &transport.private_key_file,
            ] {
                require(path.is_absolute(), "TLS paths must be absolute")?;
            }
        }
        if let Some(standalone) = &self.standalone {
            standalone.validate(self)?;
        }
        match &self.github.auth {
            AuthConfig::App {
                app_id,
                installation_id,
                private_key_file,
            } => {
                require(
                    *app_id > 0 && *installation_id > 0,
                    "App and installation IDs must be positive",
                )?;
                require(
                    private_key_file.is_absolute(),
                    "App private key path must be absolute",
                )?;
            }
            AuthConfig::AppEnv {
                app_id,
                installation_id,
                private_key_env,
            } => {
                require(
                    *app_id > 0 && *installation_id > 0,
                    "App IDs must be positive",
                )?;
                valid_env(private_key_env)?;
            }
            AuthConfig::PatEnv { token_env } => valid_env(token_env)?,
            AuthConfig::Pat { token_file } => {
                require(token_file.is_absolute(), "PAT file path must be absolute")?;
            }
        }
        Ok(())
    }
}

fn require(valid: bool, message: &str) -> Result<(), Error> {
    if valid {
        Ok(())
    } else {
        Err(Error::Validation(message.to_owned()))
    }
}

fn valid_env(name: &str) -> Result<(), Error> {
    require(
        !name.is_empty()
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            && !name.as_bytes()[0].is_ascii_digit(),
        "invalid credential environment variable name",
    )
}
