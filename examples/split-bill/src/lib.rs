//! A forged tool: split a bill between people, with an optional tip in percent.
//! Input:  {"people":"3","tip_percent":"10","total":"1200"}   (whole numbers, rupees)
//! Output: text such as "each pays 440 (total with tip 1320)"
//!
//! The logic lives in `split`, which is plain Rust and is tested natively with `cargo test`.
//! The two exported functions at the bottom are the sandbox ABI, built only for WebAssembly.
//! The tool has no imports, so the sandbox will accept it.
#![cfg_attr(target_arch = "wasm32", no_std)]

const OUT_CAP: usize = 128;

/// Find `"key":"digits"` in the input and return the number, or None.
fn field(input: &[u8], key: &[u8]) -> Option<u64> {
    let mut i = 0;
    while i + key.len() + 4 <= input.len() {
        if input[i] == b'"'
            && input[i + 1..].starts_with(key)
            && input[i + 1 + key.len()..].starts_with(b"\":\"")
        {
            let mut j = i + 1 + key.len() + 3;
            let mut n: u64 = 0;
            let mut any = false;
            while j < input.len() && input[j].is_ascii_digit() {
                n = n.checked_mul(10)?.checked_add((input[j] - b'0') as u64)?;
                any = true;
                j += 1;
            }
            return if any && j < input.len() && input[j] == b'"' {
                Some(n)
            } else {
                None
            };
        }
        i += 1;
    }
    None
}

struct Writer<'a> {
    buf: &'a mut [u8; OUT_CAP],
    len: usize,
}
impl Writer<'_> {
    fn bytes(&mut self, b: &[u8]) {
        for &c in b {
            if self.len < OUT_CAP {
                self.buf[self.len] = c;
                self.len += 1;
            }
        }
    }
    fn num(&mut self, mut n: u64) {
        let mut digits = [0u8; 20];
        let mut i = 20;
        if n == 0 {
            self.bytes(b"0");
            return;
        }
        while n > 0 {
            i -= 1;
            digits[i] = b'0' + (n % 10) as u8;
            n /= 10;
        }
        self.bytes(&digits[i..]);
    }
}

/// Write the answer into `out` and return its length.
pub fn split(input: &[u8], out: &mut [u8; OUT_CAP]) -> usize {
    let mut w = Writer { buf: out, len: 0 };
    match (
        field(input, b"people"),
        field(input, b"tip_percent"),
        field(input, b"total"),
    ) {
        (Some(p), Some(t), Some(total)) if p > 0 && t <= 100 && total <= 1_000_000_000 => {
            let with_tip = total + (total * t + 50) / 100; // tip rounded to the nearest rupee
            let each = with_tip.div_ceil(p); // rounded up, so the bill is always covered
            w.bytes(b"each pays ");
            w.num(each);
            w.bytes(b" (total with tip ");
            w.num(with_tip);
            w.bytes(b")");
        }
        _ => w.bytes(b"need people, tip_percent and total as whole numbers"),
    }
    w.len
}

// ---- WebAssembly entry points (the sandbox ABI) ----
#[cfg(target_arch = "wasm32")]
mod abi {
    use super::{split, OUT_CAP};
    use core::panic::PanicInfo;

    #[panic_handler]
    fn panic(_: &PanicInfo) -> ! {
        core::arch::wasm32::unreachable()
    }

    static mut HEAP: [u8; 4096] = [0; 4096];
    static mut NEXT: usize = 0;
    static mut OUT: [u8; OUT_CAP] = [0; OUT_CAP];

    #[no_mangle]
    pub extern "C" fn alloc(len: i32) -> i32 {
        unsafe {
            let start = NEXT;
            let end = start + len as usize;
            if end > 4096 {
                core::arch::wasm32::unreachable();
            }
            NEXT = end;
            (core::ptr::addr_of_mut!(HEAP) as *mut u8).add(start) as i32
        }
    }

    #[no_mangle]
    pub extern "C" fn run(ptr: i32, len: i32) -> i64 {
        unsafe {
            let input = core::slice::from_raw_parts(ptr as *const u8, len as usize);
            let out = &mut *core::ptr::addr_of_mut!(OUT);
            let n = split(input, out);
            let out_ptr = core::ptr::addr_of!(OUT) as i32;
            ((out_ptr as i64) << 32) | n as i64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn go(input: &str) -> String {
        let mut out = [0u8; OUT_CAP];
        let n = split(input.as_bytes(), &mut out);
        String::from_utf8(out[..n].to_vec()).unwrap()
    }

    #[test]
    fn splits_evenly_with_a_tip() {
        assert_eq!(
            go(r#"{"people":"3","tip_percent":"10","total":"1200"}"#),
            "each pays 440 (total with tip 1320)"
        );
    }
    #[test]
    fn rounds_each_share_up_so_the_bill_is_covered() {
        // 1000 + 0% tip between 3 = 333.33 -> 334
        assert_eq!(
            go(r#"{"people":"3","tip_percent":"0","total":"1000"}"#),
            "each pays 334 (total with tip 1000)"
        );
    }
    #[test]
    fn rounds_the_tip_to_the_nearest_rupee() {
        // 5% of 130 = 6.5 -> 7, so 137
        assert_eq!(
            go(r#"{"people":"1","tip_percent":"5","total":"130"}"#),
            "each pays 137 (total with tip 137)"
        );
    }
    #[test]
    fn key_order_does_not_matter() {
        assert_eq!(
            go(r#"{"total":"1200","people":"3","tip_percent":"10"}"#),
            go(r#"{"people":"3","tip_percent":"10","total":"1200"}"#)
        );
    }
    #[test]
    fn zero_people_is_refused_not_a_crash() {
        assert!(go(r#"{"people":"0","tip_percent":"10","total":"1200"}"#).starts_with("need"));
    }
    #[test]
    fn a_missing_or_non_numeric_field_is_refused() {
        assert!(go(r#"{"people":"3","total":"1200"}"#).starts_with("need"));
        assert!(go(r#"{"people":"three","tip_percent":"10","total":"1200"}"#).starts_with("need"));
        assert!(go("").starts_with("need"));
    }
    #[test]
    fn huge_numbers_do_not_overflow() {
        assert!(
            go(r#"{"people":"3","tip_percent":"10","total":"99999999999999999999999"}"#)
                .starts_with("need")
        );
        assert!(go(r#"{"people":"3","tip_percent":"101","total":"100"}"#).starts_with("need"));
    }
}
