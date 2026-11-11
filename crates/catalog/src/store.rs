//! The store as headless tools: search the catalogue, show an item, install and remove. Installing
//! is High, so it waits for the approval key like a payment does. What an install writes is kept
//! in installed.json, so a removal undoes exactly that.
use crate::{Catalog, Item, Kind};
use agentos_action::{Action, Tier, Tool};
use agentos_executor::Connector;
use std::path::{Path, PathBuf};

pub struct StoreTools {
    pub catalog: Catalog,
    /// the data directory: installed.json, agents/, skills/, mcp_servers.toml
    pub dir: PathBuf,
    /// the function store: a function item is written and checked here, then installed
    pub functions: Option<agentos_functions::Shared>,
    /// the Android sandbox, when there is one
    pub sandbox: Option<agentos_android::Shared>,
}

/// The examples of a function item: TOML `[[example]]` tables with `expect` and `args`.
pub fn parse_examples(text: &str) -> Result<Vec<agentos_functions::Example>, String> {
    #[derive(serde::Deserialize)]
    struct Raw {
        #[serde(default)]
        expect: String,
        #[serde(default)]
        args: std::collections::BTreeMap<String, String>,
    }
    #[derive(serde::Deserialize)]
    struct File {
        #[serde(default)]
        example: Vec<Raw>,
    }
    let f: File = toml::from_str(text).map_err(|e| format!("the examples do not parse: {e}"))?;
    Ok(f.example
        .into_iter()
        .map(|r| agentos_functions::Example {
            args: r.args,
            expect: r.expect,
        })
        .collect())
}

fn arg<'a>(a: &'a Action, name: &str) -> Result<&'a str, String> {
    a.args
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .ok_or_else(|| format!("missing {name}"))
}

/// The ids installed so far.
pub fn installed(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("installed.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

impl StoreTools {
    fn save_installed(&self, ids: &[String]) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let text = serde_json::to_string_pretty(ids).map_err(|e| e.to_string())?;
        std::fs::write(self.dir.join("installed.json"), text).map_err(|e| e.to_string())
    }

    fn item(&self, id: &str) -> Result<&Item, String> {
        self.catalog
            .find(id)
            .ok_or_else(|| format!("no store item called {id}"))
    }

    fn show(&self, id: &str) -> Result<String, String> {
        let i = self.item(id)?;
        let mut lines = vec![
            format!("{} ({})", i.name, i.id),
            format!(
                "kind: {:?}, publisher: {}, mode: {}",
                i.kind, i.publisher, i.mode
            ),
            i.summary.clone(),
        ];
        if i.kind == Kind::Agent {
            lines.push(format!("tools it may use: {}", i.tools.join(", ")));
        }
        Ok(lines.join("\n"))
    }

    fn install(&mut self, id: &str) -> Result<String, String> {
        let item = self.item(id)?.clone();
        let mut ids = installed(&self.dir);
        if ids.contains(&item.id) {
            return Ok(format!("{} is already installed", item.name));
        }
        let answer = match item.kind {
            Kind::Native => "built in: nothing to install".to_string(),
            Kind::Agent => {
                let dir = self.dir.join("agents");
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let text = format!(
                    "instructions = {:?}\ntools = [{}]\n",
                    item.instructions.clone().unwrap_or_default(),
                    item.tools
                        .iter()
                        .map(|t| format!("{t:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                std::fs::write(dir.join(format!("{}.toml", item.id)), text)
                    .map_err(|e| e.to_string())?;
                format!("installed {}", item.name)
            }
            Kind::Skill => {
                let body = item.body.clone().ok_or("this skill has no body")?;
                let dir = self.dir.join("skills");
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                std::fs::write(dir.join(format!("{}.toml", item.id)), body)
                    .map_err(|e| e.to_string())?;
                format!("installed {}", item.name)
            }
            Kind::Connector => {
                let url = item
                    .endpoint
                    .clone()
                    .ok_or("this connector has no endpoint")?;
                let mut text =
                    std::fs::read_to_string(self.dir.join("mcp_servers.toml")).unwrap_or_default();
                text.push_str(&format!(
                    "\n[[mcp_servers]]\nname = {:?}\nurl = {:?}\n",
                    item.id, url
                ));
                std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
                std::fs::write(self.dir.join("mcp_servers.toml"), text)
                    .map_err(|e| e.to_string())?;
                format!("installed {}; restart agentd to load its tools", item.name)
            }
            Kind::Android => {
                if item.apk_url.is_some() {
                    return Err(format!(
                        "{}: installing from an APK needs the download path, which is not wired yet",
                        item.name
                    ));
                }
                let package = item
                    .package
                    .clone()
                    .ok_or_else(|| format!("{} has no package name", item.name))?;
                let sb = self
                    .sandbox
                    .clone()
                    .ok_or("there is no Android sandbox on this phone")?;
                sb.borrow_mut().play_page(&package)?;
                format!(
                    "opened the store page for {}; sign in and install it there",
                    item.name
                )
            }
            Kind::Function => {
                let functions = self
                    .functions
                    .clone()
                    .ok_or("the function store is not available")?;
                let code = item.body.clone().ok_or("this function has no source")?;
                let examples = parse_examples(item.examples.as_deref().unwrap_or(""))?;
                // a failing example refuses the write: nothing is installed
                let mut f = functions.borrow_mut();
                // a function's name is an identifier: the item id with underscores
                let name = item.id.replace('-', "_");
                let version = f.write(&name, &item.summary, &item.args, &code, examples)?;
                f.install(&name, version)?;
                format!("installed {} as version {version}", item.name)
            }
            Kind::Planned => return Err(format!("{} is not installable yet", item.name)),
        };
        ids.push(item.id.clone());
        self.save_installed(&ids)?;
        Ok(answer)
    }

    fn remove(&mut self, id: &str) -> Result<String, String> {
        let mut ids = installed(&self.dir);
        if !ids.iter().any(|i| i == id) {
            return Err(format!("{id} is not installed"));
        }
        let _ = std::fs::remove_file(self.dir.join("agents").join(format!("{id}.toml")));
        let _ = std::fs::remove_file(self.dir.join("skills").join(format!("{id}.toml")));
        // a connector's block is cut from mcp_servers.toml; the blocks are written by install
        if let Ok(text) = std::fs::read_to_string(self.dir.join("mcp_servers.toml")) {
            let kept: Vec<&str> = text
                .split("\n[[mcp_servers]]")
                .enumerate()
                .filter(|(n, block)| *n == 0 || !block.contains(&format!("name = {id:?}")))
                .map(|(_, b)| b)
                .collect();
            let text = kept.join("\n[[mcp_servers]]");
            let _ = std::fs::write(self.dir.join("mcp_servers.toml"), text);
        }
        ids.retain(|i| i != id);
        self.save_installed(&ids)?;
        Ok(format!("removed {id}"))
    }
}

impl Connector for StoreTools {
    fn name(&self) -> &str {
        "store"
    }
    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool {
                name: "store_search",
                args: &["text"],
                tier: Tier::Low,
            },
            Tool {
                name: "store_show",
                args: &["id"],
                tier: Tier::Low,
            },
            Tool {
                name: "store_installed",
                args: &[],
                tier: Tier::Low,
            },
            Tool {
                name: "store_install",
                args: &["id"],
                tier: Tier::High,
            },
            Tool {
                name: "store_remove",
                args: &["id"],
                tier: Tier::Medium,
            },
        ]
    }
    fn call(&mut self, a: &Action) -> Result<String, String> {
        match a.tool.as_str() {
            "store_search" => {
                let hits = self.catalog.search(arg(a, "text")?);
                if hits.is_empty() {
                    return Ok("nothing in the store for that".into());
                }
                Ok(hits
                    .iter()
                    .map(|i| format!("{}: {} — {}", i.id, i.name, i.summary))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            "store_show" => self.show(arg(a, "id")?),
            "store_installed" => {
                let ids = installed(&self.dir);
                Ok(if ids.is_empty() {
                    "nothing installed from the store".into()
                } else {
                    ids.join(", ")
                })
            }
            "store_install" => {
                let id = arg(a, "id")?.to_string();
                self.install(&id)
            }
            "store_remove" => {
                let id = arg(a, "id")?.to_string();
                self.remove(&id)
            }
            other => Err(format!("store cannot {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tools(dir: &Path, catalog_text: &str) -> StoreTools {
        StoreTools {
            catalog: Catalog {
                items: crate::parse(catalog_text).unwrap(),
            },
            dir: dir.to_path_buf(),
            functions: None,
            sandbox: None,
        }
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

    const CATALOG: &str = r#"
[[item]]
id = "trip-recipe"
name = "Trip recipe"
kind = "skill"
summary = "Plans a trip."
body = "name = \"trip\"\n"

[[item]]
id = "helper"
name = "Helper"
kind = "agent"
summary = "Helps."
instructions = "Help."
tools = ["read_calendar"]

[[item]]
id = "maps-link"
name = "Maps"
kind = "connector"
summary = "A maps server."
endpoint = "https://maps.example/mcp"
"#;

    #[test]
    fn install_and_remove_undo_each_other() {
        let dir = std::env::temp_dir().join(format!("store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut t = tools(&dir, CATALOG);
        assert!(t
            .call(&act("store_install", &[("id", "trip-recipe")]))
            .unwrap()
            .contains("installed"));
        assert!(dir.join("skills/trip-recipe.toml").exists());
        assert!(t
            .call(&act("store_installed", &[]))
            .unwrap()
            .contains("trip-recipe"));
        t.call(&act("store_remove", &[("id", "trip-recipe")]))
            .unwrap();
        assert!(!dir.join("skills/trip-recipe.toml").exists());
        assert!(t
            .call(&act("store_installed", &[]))
            .unwrap()
            .contains("nothing installed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_agent_and_a_connector_are_written_where_the_daemon_reads_them() {
        let dir = std::env::temp_dir().join(format!("store-b-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut t = tools(&dir, CATALOG);
        t.call(&act("store_install", &[("id", "helper")])).unwrap();
        assert!(std::fs::read_to_string(dir.join("agents/helper.toml"))
            .unwrap()
            .contains("read_calendar"));
        assert!(t
            .call(&act("store_install", &[("id", "maps-link")]))
            .unwrap()
            .contains("restart"));
        assert!(std::fs::read_to_string(dir.join("mcp_servers.toml"))
            .unwrap()
            .contains("https://maps.example/mcp"));
        t.call(&act("store_remove", &[("id", "maps-link")]))
            .unwrap();
        assert!(!std::fs::read_to_string(dir.join("mcp_servers.toml"))
            .unwrap_or_default()
            .contains("maps.example"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_planned_item_is_not_installable() {
        let dir = std::env::temp_dir().join(format!("store-c-{}", std::process::id()));
        let mut t = tools(
            &dir,
            "[[item]]\nid = \"later\"\nname = \"Later\"\nkind = \"planned\"\n",
        );
        assert!(t
            .call(&act("store_install", &[("id", "later")]))
            .unwrap_err()
            .contains("not installable yet"));
        assert!(installed(&dir).is_empty());
    }
}

#[cfg(test)]
mod install_paths {
    use super::*;
    use agentos_functions::{Compiler, FunctionStore};
    use std::cell::RefCell;
    use std::rc::Rc;

    const SPLIT: &str = "fn run(args: &Args) -> Result<String, String> {\n    let each = args.num(\"total\")? / args.num(\"people\")?;\n    Ok(format!(\"{:.2}\", each))\n}";

    fn function_item(examples: &str) -> Item {
        Item {
            id: "split-bill".into(),
            name: "Split bill".into(),
            kind: Kind::Function,
            publisher: String::new(),
            category: "Tools".into(),
            summary: "Split a bill between people".into(),
            mode: "Built in".into(),
            endpoint: None,
            package: None,
            apk_url: None,
            apk_sha256: None,
            instructions: None,
            tools: vec![],
            body: Some(SPLIT.into()),
            args: vec!["total".into(), "people".into()],
            examples: Some(examples.into()),
        }
    }

    fn tools_with(
        dir: &Path,
        item: Item,
        functions: Option<agentos_functions::Shared>,
        sandbox: Option<agentos_android::Shared>,
    ) -> StoreTools {
        StoreTools {
            catalog: Catalog { items: vec![item] },
            dir: dir.to_path_buf(),
            functions,
            sandbox,
        }
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

    fn store() -> agentos_functions::Shared {
        let work = std::env::temp_dir().join(format!("store-fn-{}", std::process::id()));
        Rc::new(RefCell::new(FunctionStore::in_memory(
            &["send_sms"],
            Compiler::new("rustc".into(), work),
        )))
    }

    const GOOD: &str = "[[example]]\nargs = { total = \"900\", people = \"3\" }\nexpect = \"300.00\"\n\n[[example]]\nargs = { total = \"100\", people = \"4\" }\nexpect = \"25.00\"\n\n[[example]]\nargs = { total = \"10\", people = \"3\" }\nexpect = \"3.33\"\n";
    const WRONG: &str = "[[example]]\nargs = { total = \"900\", people = \"3\" }\nexpect = \"999\"\n\n[[example]]\nargs = { total = \"100\", people = \"4\" }\nexpect = \"25.00\"\n\n[[example]]\nargs = { total = \"10\", people = \"3\" }\nexpect = \"3.33\"\n";

    #[test]
    fn a_function_with_passing_examples_is_installed() {
        let dir = std::env::temp_dir().join(format!("store-fn-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let f = store();
        let mut t = tools_with(&dir, function_item(GOOD), Some(f.clone()), None);
        let said = t
            .call(&act("store_install", &[("id", "split-bill")]))
            .unwrap();
        assert!(said.contains("version 1"), "{said}");
        assert_eq!(
            f.borrow().funcs.get("split_bill").and_then(|x| x.installed),
            Some(1)
        );
        assert!(installed(&dir).contains(&"split-bill".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_function_with_a_failing_example_is_refused_and_nothing_is_installed() {
        let dir = std::env::temp_dir().join(format!("store-fn-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let f = store();
        let mut t = tools_with(&dir, function_item(WRONG), Some(f.clone()), None);
        assert!(t
            .call(&act("store_install", &[("id", "split-bill")]))
            .is_err());
        assert!(f.borrow().funcs.get("split_bill").is_none());
        assert!(installed(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_android_item_without_an_apk_opens_its_store_page_in_the_sandbox() {
        let dir = std::env::temp_dir().join(format!("store-and-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sb: agentos_android::Shared = Rc::new(RefCell::new(Box::new(
            agentos_android::SimSandbox::default(),
        )));
        let item = Item {
            id: "whatsapp".into(),
            name: "WhatsApp".into(),
            kind: Kind::Android,
            publisher: String::new(),
            category: "Messaging".into(),
            summary: "Messages".into(),
            mode: "Sandbox".into(),
            endpoint: None,
            package: Some("com.whatsapp".into()),
            apk_url: None,
            apk_sha256: None,
            instructions: None,
            tools: vec![],
            body: None,
            args: vec![],
            examples: None,
        };
        let mut t = tools_with(&dir, item, None, Some(sb.clone()));
        assert!(t
            .call(&act("store_install", &[("id", "whatsapp")]))
            .unwrap()
            .contains("store page"));
        let apps = sb.borrow_mut().apps().unwrap();
        assert!(apps.iter().any(|a| a.package == "com.whatsapp"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
