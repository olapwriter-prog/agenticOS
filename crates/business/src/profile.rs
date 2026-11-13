//! The business profile: one TOML file per owner. Nothing about a particular shop is in the
//! code; menu, prices, tax scheme, platforms and riders all come from here.
use crate::util::{rs, Paise};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub address: String,
    pub city: String,
    pub state: String,
    pub state_code: String,
    pub gstin: String,
    pub fssai: String,
    /// Where the kitchen is (for the rider's distance and the tracking message).
    pub lat: f64,
    pub lon: f64,
    /// "restaurant_5": 5% on food, no input tax credit. "regular_18": 18%, credit allowed.
    pub gst_scheme: String,
    pub food_gst_pct: f64,
    pub sac: String,
    pub invoice_prefix: String,
    /// Charged on a direct (WhatsApp) order, in rupees.
    pub delivery_fee: f64,
    pub rider_pay_per_delivery: f64,
    pub rider_pay_per_km: f64,
    /// Direct orders further than this are refused (platforms have their own riders).
    pub max_delivery_km: f64,
    /// A customer's name, phone and address are erased this many days after their last order.
    pub retain_customer_days: i64,
    /// Erase them automatically when the day turns (the owner's standing rule). If false, the
    /// phone only reminds, and erasing waits for the approval key.
    pub auto_erase: bool,
    /// Standing rules the owner sets once: bill and tracking go to the customer who ordered.
    pub auto_bill: bool,
    pub auto_tracking: bool,
    /// Extra stock bought over the forecast, percent.
    pub buffer_pct: f64,
    pub menu: Vec<MenuItem>,
    pub ingredients: Vec<Ingredient>,
    pub platforms: Vec<Platform>,
    pub riders: Vec<Rider>,
    /// Places delivered to, with the road distance from the kitchen.
    pub localities: Vec<Locality>,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            name: "My shop".into(),
            address: String::new(),
            city: String::new(),
            state: String::new(),
            state_code: String::new(),
            gstin: String::new(),
            fssai: String::new(),
            lat: 0.0,
            lon: 0.0,
            gst_scheme: "restaurant_5".into(),
            food_gst_pct: 5.0,
            sac: "996331".into(),
            invoice_prefix: "INV/".into(),
            delivery_fee: 0.0,
            rider_pay_per_delivery: 0.0,
            rider_pay_per_km: 0.0,
            max_delivery_km: 8.0,
            retain_customer_days: 30,
            auto_erase: true,
            auto_bill: true,
            auto_tracking: true,
            buffer_pct: 10.0,
            menu: vec![],
            ingredients: vec![],
            platforms: vec![],
            riders: vec![],
            localities: vec![],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MenuItem {
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub category: String,
    /// Rupees, before GST.
    pub price: f64,
    /// Other things customers call it.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Ingredient name -> quantity per plate, in that ingredient's unit.
    #[serde(default)]
    pub recipe: BTreeMap<String, f64>,
    /// How often it is ordered against the others (only the simulator reads this).
    #[serde(default = "one")]
    pub popularity: f64,
}
fn one() -> f64 {
    1.0
}
#[derive(Debug, Clone, Deserialize)]
pub struct Locality {
    pub name: String,
    pub km: f64,
    #[serde(default)]
    pub lat: f64,
    #[serde(default)]
    pub lon: f64,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Ingredient {
    pub name: String,
    pub unit: String,
    /// Units in one pack as bought.
    pub pack: f64,
    /// Rupees per pack, before GST.
    pub pack_price: f64,
    #[serde(default)]
    pub gst_pct: f64,
    #[serde(default)]
    pub supplier: String,
}
#[derive(Debug, Clone, Deserialize)]
pub struct Platform {
    pub name: String,
    pub commission_pct: f64,
    /// GST the platform charges on its commission.
    #[serde(default = "eighteen")]
    pub commission_gst_pct: f64,
    /// Income tax the platform deducts from the payout.
    #[serde(default)]
    pub tds_pct: f64,
}
fn eighteen() -> f64 {
    18.0
}
#[derive(Debug, Clone, Deserialize)]
pub struct Rider {
    pub id: String,
    pub name: String,
    pub phone: String,
}

impl Profile {
    pub fn parse(text: &str) -> Result<Profile, String> {
        let p: Profile = toml::from_str(text).map_err(|e| format!("business profile: {e}"))?;
        if p.menu.is_empty() {
            return Err("business profile: the menu is empty".into());
        }
        for m in &p.menu {
            for ing in m.recipe.keys() {
                if !p.ingredients.iter().any(|i| &i.name == ing) {
                    return Err(format!(
                        "business profile: {} uses {ing}, which is not in [[ingredients]]",
                        m.name
                    ));
                }
            }
        }
        Ok(p)
    }
    pub fn load(path: &std::path::Path) -> Result<Profile, String> {
        Self::parse(&std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?)
    }
    pub fn item(&self, code: &str) -> Option<&MenuItem> {
        self.menu.iter().find(|m| m.code.eq_ignore_ascii_case(code))
    }
    pub fn price(&self, code: &str) -> Paise {
        self.item(code).map(|m| rs(m.price)).unwrap_or(0)
    }
    pub fn platform(&self, name: &str) -> Option<&Platform> {
        self.platforms.iter().find(|p| p.name.eq_ignore_ascii_case(name))
    }
    pub fn rider(&self, id: &str) -> Option<&Rider> {
        self.riders
            .iter()
            .find(|r| r.id.eq_ignore_ascii_case(id) || r.name.eq_ignore_ascii_case(id))
    }
    pub fn locality(&self, name: &str) -> Option<&Locality> {
        let n = name.trim().to_lowercase();
        self.localities.iter().find(|l| l.name.to_lowercase() == n)
    }
    pub fn itc_allowed(&self) -> bool {
        self.gst_scheme == "regular_18"
    }
}
