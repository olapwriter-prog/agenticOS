//! Against files made by the usual producers (tests/make_fixtures.py: python-docx, openpyxl
//! recalculated by LibreOffice, python-pptx, reportlab, LibreOffice's PDF export).
use crate::*;

fn fx(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

#[test]
fn word_headings_paragraphs_and_tables() {
    let d = read_docx(&fx("agreement.docx")).unwrap();
    let titles: Vec<&str> = d.parts.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, vec!["Service Agreement", "1. Payment terms", "2. Delivery"]);
    assert_eq!(d.parts[0].text, "This agreement is between Lakshmi Widgets and Patil Stores.");
    assert!(d.parts[1].text.contains("payable within 30 days"));
    let delivery = &d.parts[2].text;
    assert!(delivery.contains("Item | Qty | Price\nWidget | 10 | 500\nRepair | 2 | 1500"), "{delivery}");
    assert!(delivery.ends_with("Signed in Bengaluru."), "text after the table stays in order: {delivery}");
}

#[test]
fn excel_sheets_with_formula_values_and_types() {
    let (d, tables) = read_xlsx(&fx("sales.xlsx")).unwrap();
    assert_eq!(tables.iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), vec!["Sales", "Expenses"]);
    let sales = &tables[0].1;
    assert_eq!(sales[0], vec!["Month", "Units", "Revenue"]);
    assert_eq!(sales[4], vec!["Total", "360", "180000"], "formulas read as the values last computed");
    let exp = &tables[1].1;
    assert_eq!(exp[1], vec!["City Power", "4720.5", "TRUE"]);
    assert_eq!(exp[2], vec!["FiberNet", "1179", "FALSE"]);
    assert_eq!(d.parts[1].title, "Sheet Expenses");
}

#[test]
fn powerpoint_slides_in_order() {
    let d = read_pptx(&fx("review.pptx")).unwrap();
    let titles: Vec<&str> = d.parts.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, vec!["Slide 1: Quarterly review", "Slide 2: Revenue", "Slide 3: Next steps"]);
    assert_eq!(d.parts[1].text, "Revenue grew 25% in October\nNovember dipped with the festival season");
    assert_eq!(d.parts[2].text, "Open a store in Mysuru\nHire two technicians");
}

#[test]
fn pdf_pages_from_two_producers() {
    let d = read_pdf(&fx("statement.pdf")).unwrap();
    assert_eq!(d.parts.len(), 2);
    assert!(d.parts[0].text.contains("Opening balance: 1,50,000.00"), "{:?}", d.parts[0]);
    assert!(d.parts[1].text.contains("closing balance 1,48,821.00"), "{:?}", d.parts[1]);
    let a = read_pdf(&fx("agreement.pdf")).unwrap();
    let all: String = a.parts.iter().map(|p| p.text.clone()).collect::<Vec<_>>().join("\n");
    assert!(all.contains("Payment terms") && all.contains("30 days"), "LibreOffice's PDF: {all}");
}

#[test]
fn a_question_picks_the_part_that_answers_it() {
    let d = read_docx(&fx("agreement.docx")).unwrap();
    let p = pick(&d, "when are invoices due, late payment interest", 400);
    assert_eq!(p[0].title, "1. Payment terms");
    let p = pick(&read_pptx(&fx("review.pptx")).unwrap(), "plans for next quarter", 400);
    assert_eq!(p[0].title, "Slide 3: Next steps");
    // within budget, even when cut
    let p = pick(&d, "", 20);
    assert!(p.iter().map(|x| tokens_of(&x.text) + tokens_of(&x.title)).sum::<usize>() <= 22);
}

#[test]
fn the_tools_stay_inside_their_folder() {
    let mut docs = Docs { dir: fx(""), budget: 600 };
    let call = |d: &mut Docs, tool: &str, args: &[(&str, &str)]| d.call(&Action { tool: tool.into(), args: args.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() });
    let list = call(&mut docs, "documents", &[]).unwrap();
    let (ui, text) = Ui::from_reply(&list).unwrap();
    assert_eq!(ui.cards.len(), 5, "{text}");
    assert!(text.contains("sales.xlsx [Excel]: 2 sheets"), "{text}");
    let r = call(&mut docs, "read_document", &[("name", "agreement.docx"), ("question", "delivery to Pune")]).unwrap();
    assert!(Ui::from_reply(&r).unwrap().1.contains("7 working days"));
    // by stem, and refusing paths
    assert!(call(&mut docs, "read_document", &[("name", "review"), ("question", "")]).is_ok());
    assert!(call(&mut docs, "read_document", &[("name", "../src/lib.rs"), ("question", "")]).unwrap_err().contains("not a path"));
    assert!(call(&mut docs, "read_document", &[("name", "nothing"), ("question", "")]).unwrap_err().contains("no document"));
}
