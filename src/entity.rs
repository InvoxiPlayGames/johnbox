use std::{
    str::FromStr,
    sync::atomic::{AtomicBool, AtomicI64},
};

use serde::{Deserialize, Serialize};
use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Stroke, Transform};
use tokio::io::Interest;

use crate::{acl::Acl, Role};

#[derive(Serialize, Debug)]
pub struct JBEntity(pub JBType, pub JBObject, pub JBAttributes);

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct JBObject {
    pub key: String,
    pub val: Option<JBValue>,
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

#[inline(always)]
const fn one() -> usize {
    1
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct JBDoodle {
    #[serde(default)]
    pub colors: Vec<csscolorparser::Color>,
    #[serde(default)]
    pub live: bool,
    #[serde(default)]
    pub max_points: usize,
    #[serde(default = "one")]
    pub max_layer: usize,
    #[serde(default)]
    pub size: JBSize,
    pub weights: Option<Vec<u32>>,
    #[serde(default)]
    pub lines: Vec<JBLine>,
}

impl JBDoodle {
    pub fn render(&self) -> Pixmap {
        let mut layers =
            vec![Pixmap::new(self.size.width, self.size.height).unwrap(); self.max_layer];

        for line in self.lines.iter() {
            if !line.points.is_empty() {
                let layer = layers.get_mut(line.layer).unwrap();
                let mut path = PathBuilder::new();
                let mut points = line.points.iter();
                let move_to = points.next().unwrap();
                path.move_to(move_to.x, move_to.y);

                for point in points {
                    path.line_to(point.x, point.y);
                }

                let path = path.finish().unwrap();
                let line_color = line.color.to_rgba8();
                let paint = Paint {
                    shader: tiny_skia::Shader::SolidColor(Color::from_rgba8(
                        line_color[0],
                        line_color[1],
                        line_color[2],
                        line_color[3],
                    )),
                    anti_alias: true,
                    ..Default::default()
                };
                let stroke = Stroke {
                    width: line.weight * 2.0,
                    line_cap: tiny_skia::LineCap::Round,
                    ..Default::default()
                };
                layer.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }

        let mut layers_iter = layers.iter_mut();

        let first_layer = layers_iter.next().unwrap();
        for layer in layers_iter {
            first_layer.draw_pixmap(
                0,
                0,
                layer.as_ref(),
                &tiny_skia::PixmapPaint {
                    opacity: 1.0,
                    ..Default::default()
                },
                Transform::identity(),
                None,
            );
        }

        layers.swap_remove(0)
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct JBLine {
    color: csscolorparser::Color,
    weight: f32,
    layer: usize,
    points: Vec<JBPoint>,
    #[serde(default)]
    pub index: usize,
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy)]
struct JBPoint {
    x: f32,
    y: f32,
}

#[derive(Deserialize, Serialize, Debug, Default, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct JBSize {
    width: u32,
    height: u32,
}
