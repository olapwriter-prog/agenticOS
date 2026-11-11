//! The camera and the photo editor. Take a picture, see what it is like, get suggested edits, and
//! apply them with one tap. Every edit is a new file, so the original is never lost.
//!
//! What it does without a model: it reads brightness, contrast, colour and cast, and suggests the
//! edits that would help (a dark photo: brighten; a flat one: enhance; dull colours: vivid). With a
//! vision model (`vision_url`, an OpenAI-style chat endpoint that takes images, such as
//! llama-server with a vision projector), it also says what is in the photo.
//!
//! Every answer is a screen (agentos-screen): the photo, what it is like, and the suggestions as
//! buttons, so the UI is generated here and drawn the same way on any screen.
use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;
use agentos_screen::{Card, Ui};
use image::{DynamicImage, GenericImageView, ImageReader, Rgb, RgbImage};
use std::path::{Path, PathBuf};

/// Takes a picture into a file.
pub trait Camera {
    fn capture(&mut self, path: &Path) -> Result<(), String>;
}

/// A scene drawn in code, for tests and the simulator: a sky, a sun and hills, a little dark and
/// flat on purpose so the suggestions have something to say.
#[derive(Default)]
pub struct SimCamera {
    pub shots: u32,
}

impl Camera for SimCamera {
    fn capture(&mut self, path: &Path) -> Result<(), String> {
        self.shots += 1;
        let (w, h) = (960u32, 720u32);
        let shift = (self.shots % 5) as f32 * 0.04;
        let img = RgbImage::from_fn(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let hill = 0.62 + 0.08 * (fx * 7.0 + shift * 10.0).sin();
            let sun = ((fx - 0.72).powi(2) + (fy - 0.25).powi(2)).sqrt() < 0.07;
            let (r, g, b) = if sun {
                (0.85, 0.75, 0.45)
            } else if fy > hill {
                (0.18, 0.32 - (fy - hill) * 0.2, 0.16)
            } else {
                (0.30 + fy * 0.15, 0.38 + fy * 0.1, 0.55 - fy * 0.1)
            };
            // dim and flat: what a phone sensor gives indoors
            let f = |c: f32| ((c * 0.75 + 0.06) * 255.0).clamp(0.0, 255.0) as u8;
            Rgb([f(r), f(g), f(b)])
        });
        img.save(path).map_err(|e| e.to_string())
    }
}

/// A camera through a command, such as libcamera's GStreamer source on the phone:
/// `gst-launch-1.0 -q libcamerasrc num-buffers=1 ! videoconvert ! jpegenc ! filesink location={path}`.
/// `{path}` is replaced with the file to write. Not yet run on a phone (plan task C4/G).
pub struct CommandCamera {
    pub command: String,
}

impl Camera for CommandCamera {
    fn capture(&mut self, path: &Path) -> Result<(), String> {
        let line = self.command.replace("{path}", &path.display().to_string());
        let mut parts = line.split_whitespace();
        let prog = parts.next().ok_or("no camera command")?;
        let out = std::process::Command::new(prog)
            .args(parts)
            .output()
            .map_err(|e| format!("{prog}: {e}"))?;
        if !out.status.success() || !path.exists() {
            return Err(format!(
                "the camera did not take a picture: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }
}

/// What a photo is like, from its pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    /// mean luma, 0 to 1
    pub brightness: f32,
    /// standard deviation of luma, 0 to about 0.5
    pub contrast: f32,
    /// mean saturation, 0 to 1
    pub saturation: f32,
    /// red minus blue, -1 to 1: positive is warm
    pub warmth: f32,
    pub width: u32,
    pub height: u32,
}

pub fn look(img: &DynamicImage) -> Look {
    let small = img.thumbnail(160, 160).to_rgb8();
    let n = (small.width() * small.height()).max(1) as f32;
    let (mut sum, mut sq, mut sat, mut warm) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for p in small.pixels() {
        let [r, g, b] = p.0.map(|c| c as f32 / 255.0);
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        sum += y;
        sq += y * y;
        let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
        sat += if mx > 0.0 { (mx - mn) / mx } else { 0.0 };
        warm += r - b;
    }
    let mean = sum / n;
    let (w, h) = img.dimensions();
    Look {
        brightness: mean,
        contrast: (sq / n - mean * mean).max(0.0).sqrt(),
        saturation: sat / n,
        warmth: warm / n,
        width: w,
        height: h,
    }
}

/// The edits that would help, best first, each with why.
pub fn suggest(l: &Look) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if l.brightness < 0.38 {
        out.push((
            "brighten",
            format!("it is dark ({:.0}% brightness)", l.brightness * 100.0),
        ));
    }
    if l.contrast < 0.16 {
        out.push(("enhance", "it looks flat: low contrast".into()));
    }
    if l.saturation < 0.28 {
        out.push(("vivid", "the colours are dull".into()));
    }
    if l.warmth < -0.08 {
        out.push(("warm", "it has a cold, blue cast".into()));
    } else if l.warmth > 0.18 {
        out.push(("cool", "it has a warm, orange cast".into()));
    }
    if out.is_empty() {
        out.push(("enhance", "a light touch-up".into()));
    }
    out.push(("bw", "a black and white version".into()));
    if l.width != l.height {
        out.push(("square", "a square crop for sharing".into()));
    }
    out
}

/// A sentence about the photo when no vision model is set.
pub fn describe(l: &Look) -> String {
    let light = if l.brightness < 0.35 {
        "Dark"
    } else if l.brightness > 0.65 {
        "Bright"
    } else {
        "Balanced"
    };
    let colour = if l.saturation < 0.25 {
        "muted colours"
    } else if l.saturation > 0.5 {
        "strong colours"
    } else {
        "natural colours"
    };
    let tone = if l.warmth < -0.08 {
        ", cool tones"
    } else if l.warmth > 0.18 {
        ", warm tones"
    } else {
        ""
    };
    format!(
        "{light} photo with {colour}{tone}, {}×{}",
        l.width, l.height
    )
}

pub const EDITS: [&str; 8] = [
    "enhance", "brighten", "vivid", "bw", "warm", "cool", "square", "rotate",
];

fn label(edit: &str) -> &'static str {
    match edit {
        "enhance" => "Enhance",
        "brighten" => "Brighten",
        "vivid" => "Vivid",
        "bw" => "Black & white",
        "warm" => "Warm",
        "cool" => "Cool",
        "square" => "Square",
        "rotate" => "Rotate",
        _ => "Edit",
    }
}

/// Stretch each channel so its darkest 1% is black and brightest 1% is white.
fn auto_levels(img: &RgbImage) -> RgbImage {
    let mut lo = [0u8; 3];
    let mut hi = [255u8; 3];
    for (c, (l, h)) in lo.iter_mut().zip(hi.iter_mut()).enumerate() {
        let mut hist = [0u32; 256];
        for p in img.pixels() {
            hist[p.0[c] as usize] += 1;
        }
        let total: u32 = hist.iter().sum();
        let cut = total / 100;
        let mut acc = 0;
        *l = hist
            .iter()
            .position(|&n| {
                acc += n;
                acc > cut
            })
            .unwrap_or(0) as u8;
        acc = 0;
        *h = 255
            - hist
                .iter()
                .rev()
                .position(|&n| {
                    acc += n;
                    acc > cut
                })
                .unwrap_or(0) as u8;
    }
    RgbImage::from_fn(img.width(), img.height(), |x, y| {
        let p = img.get_pixel(x, y).0;
        Rgb(std::array::from_fn(|c| {
            let span = (hi[c] as f32 - lo[c] as f32).max(1.0);
            (((p[c] as f32 - lo[c] as f32) / span) * 255.0).clamp(0.0, 255.0) as u8
        }))
    })
}

/// Scale saturation around each pixel's grey.
fn saturate(img: &RgbImage, k: f32) -> RgbImage {
    RgbImage::from_fn(img.width(), img.height(), |x, y| {
        let p = img.get_pixel(x, y).0.map(|c| c as f32);
        let grey = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
        Rgb(p.map(|c| (grey + (c - grey) * k).clamp(0.0, 255.0) as u8))
    })
}

fn map(img: &RgbImage, f: impl Fn([f32; 3]) -> [f32; 3]) -> RgbImage {
    RgbImage::from_fn(img.width(), img.height(), |x, y| {
        let p = img.get_pixel(x, y).0.map(|c| c as f32 / 255.0);
        Rgb(f(p).map(|c| (c * 255.0).clamp(0.0, 255.0) as u8))
    })
}

/// Apply one edit to an image.
pub fn apply(img: &DynamicImage, edit: &str) -> Result<DynamicImage, String> {
    let rgb = img.to_rgb8();
    let out = match edit {
        "enhance" => saturate(&auto_levels(&rgb), 1.12),
        "brighten" => map(&rgb, |p| p.map(|c| c.powf(0.7))),
        "vivid" => saturate(&rgb, 1.4),
        "bw" => map(&rgb, |p| {
            let y = 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
            [y, y, y]
        }),
        "warm" => map(&rgb, |p| [p[0] * 1.08 + 0.02, p[1] * 1.02, p[2] * 0.9]),
        "cool" => map(&rgb, |p| [p[0] * 0.92, p[1], p[2] * 1.08 + 0.02]),
        "square" => {
            let (w, h) = rgb.dimensions();
            let s = w.min(h);
            image::imageops::crop_imm(&rgb, (w - s) / 2, (h - s) / 2, s, s).to_image()
        }
        "rotate" => image::imageops::rotate90(&rgb),
        other => return Err(format!("no edit called {other}; try {}", EDITS.join(", "))),
    };
    Ok(DynamicImage::ImageRgb8(out))
}

/// Ask a vision model what is in the photo. None when no model is set or it does not answer.
pub fn caption_by_model(url: &str, path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let body = serde_json::json!({
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": "Describe this photo in one short sentence."},
            {"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{}", b64(&bytes))}}
        ]}],
        "max_tokens": 60,
        "temperature": 0.2
    });
    let resp = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .post(&format!(
            "{}/v1/chat/completions",
            url.trim_end_matches('/')
        ))
        .send_json(body)
        .ok()?;
    let v: serde_json::Value = resp.into_json().ok()?;
    v["choices"][0]["message"]["content"]
        .as_str()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 4);
    for ch in bytes.chunks(3) {
        let b = [ch[0], *ch.get(1).unwrap_or(&0), *ch.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        for i in 0..4 {
            out.push(if i <= ch.len() {
                A[((n >> (18 - 6 * i)) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

/// The camera and the photos as tools.
pub struct Photos {
    pub dir: PathBuf,
    pub camera: Box<dyn Camera>,
    /// an OpenAI-style vision endpoint, if any
    pub vision_url: Option<String>,
}

fn arg<'a>(a: &'a Action, name: &str) -> Result<&'a str, String> {
    a.args
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| format!("missing {name}"))
}

impl Photos {
    fn path_of(&self, name: &str) -> Result<PathBuf, String> {
        let name = name.trim();
        if name.is_empty() || name.contains('/') || name.contains("..") {
            return Err(format!("no photo called {name:?}"));
        }
        let file = if name.ends_with(".jpg") {
            name.to_string()
        } else {
            format!("{name}.jpg")
        };
        let p = self.dir.join(file);
        if p.exists() {
            Ok(p)
        } else {
            Err(format!("no photo called {name}"))
        }
    }

    fn list(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| n.ends_with(".jpg"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
            .into_iter()
            .map(|n| n.trim_end_matches(".jpg").to_string())
            .collect()
    }

    /// The screen for one photo: the picture, what it is like, and suggested edits as buttons.
    fn screen(&self, name: &str, title: &str) -> Result<Ui, String> {
        let path = self.path_of(name)?;
        let img = ImageReader::open(&path)
            .map_err(|e| e.to_string())?
            .decode()
            .map_err(|e| e.to_string())?;
        let l = look(&img);
        let caption = self
            .vision_url
            .as_deref()
            .and_then(|u| caption_by_model(u, &path))
            .unwrap_or_else(|| describe(&l));
        let mut photo = Card::new(name.to_string())
            .image(path.display().to_string())
            .line(caption);
        photo = photo.metric(format!("{:.0}%", l.brightness * 100.0), "brightness", "");
        let mut ui = Ui::new(title.to_string()).kind("photo").card(photo);
        let mut ideas = Card::new("Suggested edits").subtitle("Tap one: the original is kept");
        for (edit, why) in suggest(&l).into_iter().take(4) {
            ideas = ideas
                .line(format!("{}: {why}", label(edit)))
                .button(label(edit), format!("edit photo {name} {edit}"));
        }
        ui = ui.card(ideas);
        Ok(ui
            .action("All photos", "photos")
            .action("Take another", "take a photo"))
    }

    fn take(&mut self) -> Result<Ui, String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let n = self
            .list()
            .iter()
            .filter(|n| n.starts_with("IMG_") && !n.contains('-'))
            .count()
            + 1;
        let name = format!("IMG_{n:04}");
        let path = self.dir.join(format!("{name}.jpg"));
        self.camera.capture(&path)?;
        self.screen(&name, "Photo taken")
    }

    fn edit(&self, name: &str, edit: &str) -> Result<Ui, String> {
        let path = self.path_of(name)?;
        let img = ImageReader::open(&path)
            .map_err(|e| e.to_string())?
            .decode()
            .map_err(|e| e.to_string())?;
        let out = apply(&img, edit)?;
        let new_name = format!("{}-{edit}", name.trim_end_matches(".jpg"));
        let new_path = self.dir.join(format!("{new_name}.jpg"));
        out.to_rgb8().save(&new_path).map_err(|e| e.to_string())?;
        let mut ui = self.screen(&new_name, &format!("{} applied", label(edit)))?;
        ui = ui.action("See the original", format!("show photo {name}"));
        Ok(ui)
    }

    fn gallery(&self) -> Ui {
        let names = self.list();
        let mut ui = Ui::new("Photos")
            .kind("photo")
            .subtitle(format!("{} on this phone", names.len()));
        if names.is_empty() {
            ui = ui.card(Card::new("No photos yet").line("Say \"take a photo\" to start."));
        }
        for n in names.iter().rev().take(6) {
            if let Ok(p) = self.path_of(n) {
                ui = ui.card(
                    Card::new(n.clone())
                        .image(p.display().to_string())
                        .button("Open", format!("show photo {n}")),
                );
            }
        }
        ui.action("Take a photo", "take a photo")
    }
}

impl Connector for Photos {
    fn name(&self) -> &str {
        "photos"
    }
    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "camera_take",
                args: &[],
                tier: Tier::Low,
            },
            Tool {
                name: "photo_list",
                args: &[],
                tier: Tier::Low,
            },
            Tool {
                name: "photo_show",
                args: &["photo"],
                tier: Tier::Low,
            },
            // an edit writes a new file and keeps the original, so it is Low
            Tool {
                name: "photo_edit",
                args: &["photo", "edit"],
                tier: Tier::Low,
            },
        ]
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        let ui = match a.tool.as_str() {
            "camera_take" => self.take()?,
            "photo_list" => self.gallery(),
            "photo_show" => self.screen(arg(a, "photo")?, "Photo")?,
            "photo_edit" => self.edit(arg(a, "photo")?, arg(a, "edit")?)?,
            other => return Err(format!("photos cannot {other}")),
        };
        Ok(ui.to_reply())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vision-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn act(tool: &str, args: &[(&str, &str)]) -> Action {
        Action {
            tool: tool.into(),
            args: args
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn a_dark_flat_photo_gets_brighten_and_enhance_suggested() {
        let d = tmp("look");
        let p = d.join("a.jpg");
        SimCamera::default().capture(&p).unwrap();
        let l = look(&ImageReader::open(&p).unwrap().decode().unwrap());
        let ideas: Vec<&str> = suggest(&l).iter().map(|(e, _)| *e).collect();
        assert!(
            ideas.contains(&"brighten") || ideas.contains(&"enhance"),
            "{l:?} {ideas:?}"
        );
        assert!(
            ideas.contains(&"square"),
            "a 4:3 photo is offered a square crop"
        );
    }

    #[test]
    fn each_edit_changes_the_picture_the_way_it_says() {
        let d = tmp("edits");
        let p = d.join("a.jpg");
        SimCamera::default().capture(&p).unwrap();
        let img = ImageReader::open(&p).unwrap().decode().unwrap();
        let before = look(&img);
        let after = |e: &str| look(&apply(&img, e).unwrap());
        assert!(after("brighten").brightness > before.brightness);
        assert!(after("enhance").contrast > before.contrast);
        assert!(after("vivid").saturation > before.saturation);
        assert!(after("bw").saturation < 0.02);
        assert!(after("warm").warmth > before.warmth);
        let sq = apply(&img, "square").unwrap();
        assert_eq!(sq.width(), sq.height());
        let r = apply(&img, "rotate").unwrap();
        assert_eq!((r.width(), r.height()), (img.height(), img.width()));
        assert!(apply(&img, "sparkle").is_err());
    }

    #[test]
    fn take_then_edit_keeps_the_original_and_answers_with_a_screen() {
        let d = tmp("tools");
        let mut ph = Photos {
            dir: d.clone(),
            camera: Box::new(SimCamera::default()),
            vision_url: None,
        };
        let taken = Ui::from_reply(&ph.call(&act("camera_take", &[])).unwrap())
            .unwrap()
            .0;
        assert_eq!(taken.kind, "photo");
        assert!(taken.cards[0].image.ends_with("IMG_0001.jpg"));
        let ideas = &taken.cards[1];
        assert!(!ideas.actions.is_empty(), "suggested edits come as buttons");
        assert!(ideas.actions[0].say.starts_with("edit photo IMG_0001 "));
        let edited = Ui::from_reply(
            &ph.call(&act(
                "photo_edit",
                &[("photo", "IMG_0001"), ("edit", "enhance")],
            ))
            .unwrap(),
        )
        .unwrap()
        .0;
        assert!(edited.cards[0].image.ends_with("IMG_0001-enhance.jpg"));
        assert!(d.join("IMG_0001.jpg").exists(), "the original is kept");
        let gallery = Ui::from_reply(&ph.call(&act("photo_list", &[])).unwrap())
            .unwrap()
            .0;
        assert_eq!(gallery.subtitle, "2 on this phone");
        assert!(ph
            .call(&act("photo_show", &[("photo", "../etc/passwd")]))
            .is_err());
    }
}
