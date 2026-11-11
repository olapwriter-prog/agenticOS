use crate::*;

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("skillpack-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn frontmatter_with_folded_descriptions_is_read() {
    let t = "---\nname: dpop-adoption\nmetadata:\n  version: \"1.0\"\ndescription: >-\n  Implement OAuth DPoP\n  for browsers.\nlicense: MIT\n---\n# Title\nbody\n";
    let (f, body) = split_front(t);
    let fm: Front = serde_yaml::from_str(f.unwrap()).unwrap();
    assert_eq!(fm.name, "dpop-adoption");
    assert_eq!(fm.description, "Implement OAuth DPoP for browsers.");
    assert_eq!(body, "# Title\nbody\n");
    assert_eq!(split_front("# no frontmatter").0, None);
    // hand-written YAML that a strict parser rejects (an unquoted ": ")
    let fm = parse_front("name: react\ndescription: React rules: avoid barrel imports,\n  memoise renders.\nlicense: MIT\n");
    assert_eq!((fm.name.as_str(), fm.description.as_str()), ("react", "React rules: avoid barrel imports, memoise renders."));
    assert_eq!(fm.license.and_then(|v| v.as_str().map(String::from)).as_deref(), Some("MIT"));
}

#[test]
fn licenses_are_recognised_from_texts_and_ids() {
    assert_eq!(detect_license("                                 Apache License\n                           Version 2.0, January 2004"), Some("Apache-2.0"));
    assert_eq!(detect_license("MIT License\n\nCopyright (c) Microsoft"), Some("MIT"));
    assert_eq!(detect_license("Permission is hereby granted, free of charge, to any person ... The above copyright notice"), Some("MIT"));
    assert_eq!(detect_license("Attribution 4.0 International"), Some("CC-BY-4.0"));
    assert_eq!(detect_license("MIT"), Some("MIT"));
    assert_eq!(detect_license("Complete terms in LICENSE.txt"), None);
    assert_eq!(detect_license("© 2025 Example. All rights reserved."), None);
}

#[test]
fn sections_follow_headings_but_never_split_code() {
    let md = "intro text that is long enough to stand on its own as a section of the guide here, with a few more words to be sure of it.\n\n## Install\nrun this:\n```bash\n# not a heading\npip install x\n```\nmore words to make this section long enough to stay on its own, more words, more words, and a few more after that.\n\n## Tiny\nok\n";
    let s = sections("SKILL.md", md, "guide");
    assert_eq!(s[0].title, "guide");
    assert_eq!(s[1].title, "Install");
    assert!(s[1].text.contains("# not a heading"), "a # inside code is not a heading");
    assert!(s[1].text.contains("Tiny:\nok"), "a tiny section joins the one before: {:?}", s);
    // a long section is cut at paragraph breaks
    let long = format!("## Big\n{}", (0..40).map(|i| format!("paragraph {i} {}", "word ".repeat(30))).collect::<Vec<_>>().join("\n\n"));
    let s = sections("SKILL.md", &long, "x");
    assert!(s.len() > 1 && s.iter().all(|x| x.text.len() <= 3300), "{:?}", s.iter().map(|x| x.text.len()).collect::<Vec<_>>());
    assert_eq!(s[1].title, "Big (2)");
}

fn catalog(root: &Path) {
    // a skill with its own Apache license file, a reference, and a script
    write(root, "skills/pdf/SKILL.md", "---\nname: pdf\ndescription: Read, merge and split PDF files.\nlicense: Complete terms in LICENSE.txt\n---\n# PDF\nUse pypdf to merge PDF files page by page.\n\n## Forms\nTo fill a PDF form, read the fields first, then write values for each field.\n");
    write(root, "skills/pdf/LICENSE.txt", "                                 Apache License\n                           Version 2.0, January 2004\n");
    write(root, "skills/pdf/reference.md", "# Encryption\nTo encrypt a PDF, set a user password and an owner password.\n");
    write(root, "skills/pdf/scripts/merge.py", "print('never run')\n");
    // frontmatter license only
    write(root, "skills/react/SKILL.md", "---\nname: react-best-practices\ndescription: React performance rules: avoid barrel imports, memoise expensive renders.\nlicense: MIT\n---\n# Rules\nAvoid barrel imports; import from the module directly to keep bundles small.\n");
    // no license at all, and a proprietary one
    write(root, "skills/secret/SKILL.md", "---\nname: secret\ndescription: Something.\n---\n# x\ny\n");
    write(root, "skills/closed/SKILL.md", "---\nname: closed\ndescription: Something closed.\n---\n# x\ny\n");
    write(root, "skills/closed/LICENSE.txt", "© 2025 Example Corp. All rights reserved. Do not redistribute.");
    // no description
    write(root, "skills/broken/SKILL.md", "---\nname: broken\n---\n# x\n");
}

#[test]
fn only_redistributable_skills_are_kept_with_their_evidence() {
    let root = tmp("cat");
    catalog(&root);
    let src = Source { name: "acme".into(), repo: "https://github.com/acme/skills".into(), commit: "abc123".into(), repo_license: None };
    let (kept, skipped) = import(&root, &src);
    let ids: Vec<&str> = kept.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec!["acme/pdf", "acme/react-best-practices"]);
    let pdf = &kept[0];
    assert_eq!((pdf.license.as_str(), pdf.license_evidence.as_str()), ("Apache-2.0", "LICENSE.txt in the skill"));
    assert_eq!(kept[1].license_evidence, "license field of SKILL.md");
    assert!(pdf.sections.iter().any(|s| s.file == "reference.md" && s.text.contains("owner password")), "references are kept");
    assert_eq!(pdf.other_files, vec!["scripts/merge.py"], "scripts are listed, not kept as text");
    assert_eq!(pdf.origin, "https://github.com/acme/skills/tree/abc123/skills/pdf");
    let why: Vec<(&str, &str)> = skipped.iter().map(|s| (s.path.as_str(), s.reason.as_str())).collect();
    assert!(why.contains(&("skills/secret", "no license found")), "{why:?}");
    assert!(why.iter().any(|(p, r)| *p == "skills/closed" && r.contains("not a license we can redistribute")), "{why:?}");
    assert!(why.contains(&("skills/broken", "no description")), "{why:?}");
    // a repository license covers skills without their own
    let src2 = Source { repo_license: Some(("MIT".into(), "repository LICENSE".into())), ..src };
    let (kept2, _) = import(&root, &src2);
    assert!(kept2.iter().any(|s| s.id == "acme/secret" && s.license_evidence == "repository LICENSE"));
    assert!(!kept2.iter().any(|s| s.id == "acme/closed"), "a skill's own restrictive license wins over the repository's");
}

#[test]
fn the_pack_is_searched_and_cut_to_a_budget() {
    let root = tmp("lib");
    catalog(&root);
    let pack = root.join("pack");
    let src = Source { name: "acme".into(), repo: "r".into(), commit: "c".into(), repo_license: None };
    let (kept, _) = import(&root, &src);
    assert_eq!(write_pack(&pack, &kept, ("acme", "r at c")).unwrap(), 2);
    let lib = Library::open(&pack).unwrap();
    assert_eq!(lib.search("merge pdf files", 1)[0].0.id, "acme/pdf");
    assert_eq!(lib.search("react bundle size barrel imports", 1)[0].0.id, "acme/react-best-practices");
    assert!(lib.search("kubernetes", 3).is_empty());
    assert_eq!(lib.find("pdf").unwrap().id, "acme/pdf");
    let s = lib.load(lib.find("acme/pdf").unwrap()).unwrap();
    // the question picks the encryption reference, within the budget
    let (secs, used) = excerpt(&s, "how do I encrypt a pdf with a password", 60);
    assert!(secs.iter().any(|x| x.text.contains("owner password")), "{secs:?}");
    assert!(used <= 60);
    // a tiny budget still never overflows
    let (_, used) = excerpt(&s, "forms", 25);
    assert!(used <= 25);
    let t = excerpt_text(&s, &secs);
    assert!(t.starts_with("Skill pdf (Apache-2.0): Read, merge and split PDF files."));
    // re-importing a source replaces it, not duplicates it
    assert_eq!(write_pack(&pack, &kept, ("acme", "r at c")).unwrap(), 2);
}

#[test]
fn terms_drop_stop_words_and_stem_lightly() {
    assert_eq!(terms("How do I merge the PDFs?"), vec!["merge", "pdf"]);
    assert_eq!(terms("encrypting files"), vec!["encrypt", "file"]);
}
