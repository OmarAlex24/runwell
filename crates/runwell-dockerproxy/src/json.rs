//! Validate keys while deserializing, before serde_json can discard duplicates.
use crate::Error;
use serde::{
    Deserialize,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Value};
use std::{cell::Cell, collections::BTreeSet, fmt};

#[derive(Clone, Copy)]
pub(crate) enum Scope {
    Root,
    Host,
    Mount,
    Labels,
    Other,
}
impl Scope {
    fn child(self, key: &str) -> Self {
        match self {
            Self::Root if key.eq_ignore_ascii_case("HostConfig") => Self::Host,
            Self::Root if key.eq_ignore_ascii_case("Labels") => Self::Labels,
            Self::Host if key.eq_ignore_ascii_case("Mounts") => Self::Mount,
            _ => Self::Other,
        }
    }
    fn protected(self, key: &str) -> bool {
        let names: &[&str] = match self {
            Self::Root | Self::Host => &[
                "HostConfig",
                "Labels",
                "Binds",
                "Mounts",
                "CgroupParent",
                "Memory",
                "Privileged",
                "PidMode",
                "NetworkMode",
            ],
            Self::Mount => &["Type", "Source", "Target"],
            Self::Labels => &["io.runwell.job", "io.runwell.node"],
            Self::Other => &[],
        };
        names.iter().any(|name| key.eq_ignore_ascii_case(name))
    }
}

pub(crate) fn parse(bytes: &[u8], scope: Scope) -> Result<Value, Error> {
    let invalid_keys = Cell::new(false);
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let result = Seed {
        scope,
        invalid_keys: &invalid_keys,
    }
    .deserialize(&mut decoder);
    let value = result.map_err(|_| {
        if invalid_keys.get() {
            Error::JsonKeys
        } else {
            Error::Payload
        }
    })?;
    decoder.end().map_err(|_| Error::Payload)?;
    Ok(value)
}

struct Seed<'a> {
    scope: Scope,
    invalid_keys: &'a Cell<bool>,
}
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Value;
    fn deserialize<D: de::Deserializer<'de>>(self, decoder: D) -> Result<Value, D::Error> {
        if matches!(self.scope, Scope::Other) {
            Value::deserialize(decoder)
        } else {
            decoder.deserialize_any(self)
        }
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Value;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON with unambiguous ASCII attribution keys")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> Result<Value, A::Error> {
        let mut object = Map::new();
        let mut seen = BTreeSet::new();
        while let Some(key) = input.next_key::<String>()? {
            if !key.is_ascii()
                || (self.scope.protected(&key) && !seen.insert(key.to_ascii_lowercase()))
            {
                self.invalid_keys.set(true);
                return Err(de::Error::custom("ambiguous attribution key"));
            }
            let value = input.next_value_seed(Seed {
                scope: self.scope.child(&key),
                invalid_keys: self.invalid_keys,
            })?;
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut input: A) -> Result<Value, A::Error> {
        let mut array = Vec::new();
        while let Some(value) = input.next_element_seed(Seed {
            scope: self.scope,
            invalid_keys: self.invalid_keys,
        })? {
            array.push(value);
        }
        Ok(Value::Array(array))
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
}
