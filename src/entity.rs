use std::{
    str::FromStr,
    sync::atomic::{AtomicBool, AtomicI64},
};

use hex_color::HexColor;
use serde::{Deserialize, Serialize};
use tokio::io::Interest;

use crate::{acl::Acl, Role};

#[derive(Serialize, Debug)]
pub struct JBEntity(pub JBType, pub JBObject, pub JBAttributes);

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct JBObject {
    pub key: String,
    pub val: JBValue,
    pub restrictions: JBRestrictions,
    pub version: u32,
    pub from: AtomicI64,
}

#[derive(Serialize, Debug, Clone)]
#[serde(untagged)]
pub enum JBValue {
    Text(String),
    Number(f64),
    Object(serde_json::Map<String, serde_json::Value>),
    Doodle(JBDoodle),
}

#[derive(Serialize, Debug, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum JBType {
    Text,
    Number,
    Object,
    Doodle,
}

impl FromStr for JBType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "text" => Ok(Self::Text),
            "number" => Ok(Self::Number),
            "object" => Ok(Self::Object),
            "doodle" => Ok(Self::Doodle),
            _ => Err(()),
        }
    }
}

#[derive(Deserialize, Serialize, Debug)]
pub struct JBRestrictions {
    #[serde(default)]
    #[serde(skip_serializing_if = "String::is_empty")]
    #[serde(rename = "type")]
    data_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    increment: Option<f64>,
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct JBAttributes {
    pub locked: AtomicBool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub acl: Vec<Acl>,
}

impl JBAttributes {
    pub fn perms(&self, role: Role, id: i64) -> Option<Interest> {
        if role == Role::Host {
            return Some(Interest::READABLE | Interest::WRITABLE);
        }

        let mut perms: Option<Interest> = None;

        for principle in self.acl.iter() {
            if principle.principle.matches(role, id) {
                *perms.get_or_insert(principle.interest) |= principle.interest;
            }
        }

        perms
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct JBDoodle {
    #[serde(default)]
    pub colors: Vec<HexColor>,
    #[serde(default)]
    pub live: bool,
    #[serde(default)]
    pub max_points: u32,
    #[serde(default)]
    pub max_layer: u32,
    #[serde(default)]
    pub size: JBSize,
    #[serde(default)]
    pub weights: Vec<u32>,
    #[serde(default)]
    pub lines: Vec<JBLine>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct JBLine {
    color: HexColor,
    weight: u32,
    layer: u32,
    points: Vec<JBPoint>,
    #[serde(default)]
    pub index: usize,
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy)]
struct JBPoint {
    x: f64,
    y: f64,
}

#[derive(Deserialize, Serialize, Debug, Default, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct JBSize {
    width: u32,
    height: u32,
}
