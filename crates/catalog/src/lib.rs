//! The store's catalogue: what can be installed, how it runs, and what permissions it asks for.
//! A catalogue is a TOML file of [[item]] tables. Later files override earlier items with the same
//! id, so a remote catalogue can update the shipped one.
pub mod store;
pub mod update;

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// a named set of instructions with a fixed list of tools it may use
    Agent,
    /// a recipe of tool calls with parameters
    Skill,
    /// Rust compiled to WebAssembly and run in the sandbox
    Function,
    /// a remote MCP server
    Connector,
    /// an Android app in the sandbox
    Android,
    /// nothing to install: a feature that is already built in
    Native,
    /// listed so people can see it; not installable yet
    Planned,
}

#[derive(Debug, Clone, Deserialize)]
struct RawItem {
    id: String,
    name: String,
    kind: String,
    #[serde(default)]
    publisher: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    mode: String,
    endpoint: Option<String>,
    package: Option<String>,
    apk_url: Option<String>,
    apk_sha256: Option<String>,
    instructions: Option<String>,
    #[serde(default)]
    tools: Vec<String>,
    body: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    examples: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub publisher: String,
    pub category: String,
    pub summary: String,
    /// how it runs, as people read it: Headless, Sandbox, Built in, ...
    pub mode: String,
    /// connector: the MCP endpoint
    pub endpoint: Option<String>,
    /// android: the package name
    pub package: Option<String>,
    pub apk_url: Option<String>,
    pub apk_sha256: Option<String>,
    /// agent: its instructions
    pub instructions: Option<String>,
    /// agent: the tools it may use
    pub tools: Vec<String>,
    /// skill: its TOML; function: its Rust source
    pub body: Option<String>,
    /// function: its arguments
    pub args: Vec<String>,
    pub examples: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub items: Vec<Item>,
}

fn kind_of(id: &str, kind: &str) -> Result<Kind, String> {
    Ok(match kind {
        "agent" => Kind::Agent,
        "skill" => Kind::Skill,
        "function" => Kind::Function,
        "connector" => Kind::Connector,
        "android" => Kind::Android,
        "native" => Kind::Native,
        "planned" => Kind::Planned,
        other => return Err(format!("item {id}: unknown kind {other:?}")),
    })
}

fn parse_item(raw: RawItem) -> Result<Item, String> {
    let kind = kind_of(&raw.id, &raw.kind)?;
    Ok(Item {
        kind,
        id: raw.id,
        name: raw.name,
        publisher: raw.publisher,
        category: raw.category,
        summary: raw.summary,
        mode: raw.mode,
        endpoint: raw.endpoint,
        package: raw.package,
        apk_url: raw.apk_url,
        apk_sha256: raw.apk_sha256,
        instructions: raw.instructions,
        tools: raw.tools,
        body: raw.body,
        args: raw.args,
        examples: raw.examples,
    })
}

/// Parse one catalogue file.
pub fn parse(text: &str) -> Result<Vec<Item>, String> {
    #[derive(Deserialize)]
    struct File {
        #[serde(default)]
        item: Vec<RawItem>,
    }
    let f: File = toml::from_str(text).map_err(|e| e.to_string())?;
    f.item.into_iter().map(parse_item).collect()
}

impl Catalog {
    /// Read the files in order. An item in a later file replaces the earlier item with its id.
    pub fn load(paths: &[PathBuf]) -> Result<Catalog, String> {
        let mut items: Vec<Item> = Vec::new();
        for p in paths {
            let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
            for item in parse(&text).map_err(|e| format!("{}: {e}", p.display()))? {
                match items.iter_mut().find(|i| i.id == item.id) {
                    Some(old) => *old = item,
                    None => items.push(item),
                }
            }
        }
        Ok(Catalog { items })
    }

    pub fn find(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Items whose name, summary, category, mode or kind contains the text, ignoring case. The kind
    /// is searched by its name in lower case, so "agent" or "android" find those items.
    pub fn search(&self, text: &str) -> Vec<&Item> {
        let t = text.to_lowercase();
        self.items
            .iter()
            .filter(|i| {
                let kind = format!("{:?}", i.kind).to_lowercase();
                [&i.name, &i.summary, &i.category, &i.mode]
                    .iter()
                    .any(|f| f.to_lowercase().contains(&t))
                    || kind.contains(&t)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: &str = include_str!("../../../os/catalog/catalog.toml");

    fn seed() -> Catalog {
        Catalog {
            items: parse(SEED).unwrap(),
        }
    }

    #[test]
    fn the_seed_file_parses_and_ids_are_unique() {
        let c = seed();
        assert!(c.items.len() >= 40, "{}", c.items.len());
        let mut ids: Vec<&str> = c.items.iter().map(|i| i.id.as_str()).collect();
        ids.sort();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate ids");
    }

    #[test]
    fn every_agent_lists_at_least_one_tool() {
        for a in seed().items.iter().filter(|i| i.kind == Kind::Agent) {
            assert!(!a.tools.is_empty(), "{} has no tools", a.id);
            assert!(a.instructions.is_some(), "{} has no instructions", a.id);
        }
    }

    #[test]
    fn a_search_for_food_finds_zomato_and_swiggy() {
        let c = seed();
        let ids: Vec<&str> = c.search("food").iter().map(|i| i.id.as_str()).collect();
        assert!(
            ids.contains(&"zomato") && ids.contains(&"swiggy"),
            "{ids:?}"
        );
    }

    #[test]
    fn an_unknown_kind_is_an_error_that_names_the_item() {
        let e = parse("[[item]]\nid = \"oops\"\nname = \"Oops\"\nkind = \"bogus\"\n").unwrap_err();
        assert!(e.contains("oops") && e.contains("bogus"), "{e}");
    }

    #[test]
    fn a_search_by_kind_finds_that_kind() {
        let c = seed();
        // the text search also reads summaries, so it finds every agent (and a few planned items that
        // mention agents in their text)
        let found = c.search("agent");
        for a in c.items.iter().filter(|i| i.kind == Kind::Agent) {
            assert!(found.iter().any(|i| i.id == a.id), "{} not found", a.id);
        }
        assert!(c.search("android").iter().any(|i| i.id == "whatsapp"));
    }

    #[test]
    fn a_later_file_overrides_an_earlier_item() {
        let dir = std::env::temp_dir().join(format!("catalog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.toml");
        let b = dir.join("b.toml");
        std::fs::write(
            &a,
            "[[item]]\nid = \"x\"\nname = \"Old\"\nkind = \"native\"\n",
        )
        .unwrap();
        std::fs::write(
            &b,
            "[[item]]\nid = \"x\"\nname = \"New\"\nkind = \"native\"\n",
        )
        .unwrap();
        let c = Catalog::load(&[a, b]).unwrap();
        assert_eq!(c.find("x").unwrap().name, "New");
        assert_eq!(c.items.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
