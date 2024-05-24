use std::{borrow::Cow, fmt::Display, str::FromStr};

use serde::{de::Error, Deserialize, Serialize};
use tokio::io::Interest;

use crate::Role;

#[derive(Debug, Clone, Copy)]
pub struct Acl {
    pub interest: Interest,
    pub principle: Principle,
}

impl Acl {
    pub fn default_vec() -> Vec<Self> {
        vec![Self::default()]
    }
}

impl Default for Acl {
    fn default() -> Self {
        Acl {
            interest: Interest::READABLE,
            principle: Principle::Wild,
        }
    }
}

impl Display for Acl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.interest.is_priority() {
            write!(f, "s")?;
        }
        if self.interest.is_readable() {
            write!(f, "r")?;
        }
        if self.interest.is_writable() {
            write!(f, "w")?;
        }

        write!(f, " {}", self.principle)
    }
}

impl Serialize for Acl {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        format!("{}", self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Acl {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let acl: Cow<'de, str> = Deserialize::deserialize(deserializer)?;
        Acl::from_str(acl.as_ref()).map_err(|_| {
            D::Error::invalid_value(
                serde::de::Unexpected::Str(acl.as_ref()),
                &"A valid ACL principle",
            )
        })
    }
}

impl FromStr for Acl {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let split = s.split_once(' ').ok_or(())?;

        let interest = split
            .0
            .chars()
            .try_fold(None, |state, interest| match interest {
                's' => Ok(Some(
                    state.unwrap_or(Interest::PRIORITY) | Interest::PRIORITY,
                )),
                'r' => Ok(Some(
                    state.unwrap_or(Interest::READABLE) | Interest::READABLE,
                )),
                'w' => Ok(Some(
                    state.unwrap_or(Interest::WRITABLE) | Interest::WRITABLE,
                )),
                _ => return Err(()),
            })?
            .ok_or(())?;

        if split.1 == "*" {
            return Ok(Self {
                interest,
                principle: Principle::Wild,
            });
        }

        let split = split.1.split_once(':').ok_or(())?;

        let principle = match split.0 {
            "role" => Principle::Role(split.1.parse()?),
            "id" => Principle::Id(split.1.parse().or(Err(()))?),
            _ => return Err(()),
        };

        Ok(Self {
            interest,
            principle,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Principle {
    Role(Role),
    Id(i64),
    Wild,
}

impl Principle {
    pub fn matches(&self, role: Role, id: i64) -> bool {
        match *self {
            Self::Role(r) => role == r,
            Self::Id(i) => id == i,
            Self::Wild => true,
        }
    }
}

impl Display for Principle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Role(r) => write!(f, "role:{}", r),
            Self::Id(id) => write!(f, "id:{}", id),
            Self::Wild => write!(f, "*"),
        }
    }
}
