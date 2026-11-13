//! A one-page text PDF (Courier, A5), enough for a bill. No library: the file is small and fixed.

fn esc(line: &str) -> String {
    line.chars()
        .map(|c| match c {
            '₹' => "Rs ".to_string(),
            '(' | ')' | '\\' => format!("\\{c}"),
            c if c.is_ascii() && !c.is_ascii_control() => c.to_string(),
            _ => "?".to_string(),
        })
        .collect()
}

pub fn text_pdf(lines: &[String]) -> Vec<u8> {
    let (w, h, size, lead) = (420, 595, 9, 12);
    let mut content = format!("BT /F1 {size} Tf {lead} TL 28 {} Td\n", h - 36);
    for l in lines {
        content.push_str(&format!("({}) Tj T*\n", esc(l)));
    }
    content.push_str("ET\n");
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>"),
        format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>".to_string(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
    for off in offsets {
        out.extend(format!("{off:010} 00000 n \n").bytes());
    }
    out.extend(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1)
            .bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_pdf_is_well_formed() {
        let p = text_pdf(&["TAX INVOICE".into(), "Total ₹385 (paid)".into()]);
        let s = String::from_utf8(p.clone()).unwrap();
        assert!(s.starts_with("%PDF-1.4"));
        assert!(s.trim_end().ends_with("%%EOF"));
        assert!(s.contains("(Total Rs 385 \\(paid\\)) Tj"));
        // the xref offset points at the xref table
        let at: usize = s.split("startxref\n").nth(1).unwrap().lines().next().unwrap().parse().unwrap();
        assert!(s[at..].starts_with("xref"));
        // every object offset points at its object
        for (i, l) in s[at..].lines().skip(3).take(5).enumerate() {
            let off: usize = l[..10].parse().unwrap();
            assert!(s[off..].starts_with(&format!("{} 0 obj", i + 1)));
        }
    }
}
