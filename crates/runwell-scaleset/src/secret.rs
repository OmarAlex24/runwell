use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer};
use std::fmt;

/// A credential whose `Debug` and `Display` implementations always redact it.
pub struct Secret(SecretString);

impl Clone for Secret {
    fn clone(&self) -> Self {
        Self::new(self.expose())
    }
}

impl Secret {
    /// Wrap a credential, zeroizing its storage on drop.
    pub fn new(value: impl Into<String>) -> Self {
        Self(SecretString::from(value.into()))
    }

    /// Explicitly expose the credential for authentication or runner startup.
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
}
