//! The approval key on the phone itself: a hardware button (volume-down by default) held for two
//! seconds approves the one action that is waiting, as `agentd ctl press N` does over SSH.
//!
//! The button is read from its Linux input device (`/dev/input/eventN`, or a stable name under
//! `/dev/input/by-path/`). A hold counts when the key is released after `hold_ms`; a short press
//! is left to whatever else uses the key (the volume). When more than one action is waiting the
//! key approves none of them: without a screen the human could not tell which one they approved.
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

const EV_KEY: u16 = 1;
/// KEY_VOLUMEDOWN in linux/input-event-codes.h
pub const KEY_VOLUMEDOWN: u16 = 114;

/// One input event: (type, code, value, time in ms).
pub type Event = (u16, u16, i32, u64);

/// Size of `struct input_event` on this machine (24 bytes on 64-bit, 16 on 32-bit).
const EVENT_SIZE: usize = std::mem::size_of::<libc::input_event>();

/// Decode whole `struct input_event`s from a read; a partial tail is ignored.
pub fn parse(buf: &[u8]) -> Vec<Event> {
    let tv = EVENT_SIZE - 8; // the timeval, then u16 type, u16 code, i32 value
    let half = tv / 2; // seconds, then microseconds, each a C long
    let long = |b: &[u8]| -> i64 {
        if half == 8 {
            i64::from_ne_bytes(b.try_into().unwrap())
        } else {
            i32::from_ne_bytes(b.try_into().unwrap()) as i64
        }
    };
    buf.chunks_exact(EVENT_SIZE)
        .map(|e| {
            let (sec, usec) = (long(&e[..half]), long(&e[half..tv]));
            let ty = u16::from_ne_bytes([e[tv], e[tv + 1]]);
            let code = u16::from_ne_bytes([e[tv + 2], e[tv + 3]]);
            let value = i32::from_ne_bytes(e[tv + 4..tv + 8].try_into().unwrap());
            (
                ty,
                code,
                value,
                (sec.max(0) as u64) * 1000 + (usec.max(0) as u64) / 1000,
            )
        })
        .collect()
}

/// Finds holds of one key in a stream of events.
pub struct Hold {
    pub code: u16,
    pub hold_ms: u64,
    down_at: Option<u64>,
}

impl Hold {
    pub fn new(code: u16, hold_ms: u64) -> Self {
        Hold {
            code,
            hold_ms,
            down_at: None,
        }
    }
    /// Feed one event; true when it ends a long enough hold.
    pub fn feed(&mut self, (ty, code, value, t): Event) -> bool {
        if ty != EV_KEY || code != self.code {
            return false;
        }
        match value {
            1 => {
                self.down_at = Some(t);
                false
            }
            0 => self
                .down_at
                .take()
                .is_some_and(|d| t.saturating_sub(d) >= self.hold_ms),
            _ => false, // autorepeat
        }
    }
}

/// Read the button in a thread; `on_hold` runs for every hold. A missing device is retried
/// every ten seconds (input devices can appear after agentd starts).
pub fn spawn(device: PathBuf, code: u16, hold_ms: u64, on_hold: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        let mut warned = false;
        loop {
            match std::fs::File::open(&device) {
                Ok(mut f) => {
                    warned = false;
                    let mut hold = Hold::new(code, hold_ms);
                    let mut buf = vec![0u8; EVENT_SIZE * 64];
                    while let Ok(n) = f.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        for ev in parse(&buf[..n - n % EVENT_SIZE]) {
                            if hold.feed(ev) {
                                on_hold();
                            }
                        }
                    }
                }
                Err(e) if !warned => {
                    eprintln!("agentd: approval key {}: {e} (retrying)", device.display());
                    warned = true;
                }
                Err(_) => {}
            }
            std::thread::sleep(Duration::from_secs(10));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(sec: i64, usec: i64, ty: u16, code: u16, value: i32) -> Vec<u8> {
        let mut e: libc::input_event = unsafe { std::mem::zeroed() };
        e.time.tv_sec = sec as _;
        e.time.tv_usec = usec as _;
        e.type_ = ty;
        e.code = code;
        e.value = value;
        let p = &e as *const libc::input_event as *const u8;
        unsafe { std::slice::from_raw_parts(p, EVENT_SIZE) }.to_vec()
    }

    #[test]
    fn events_decode_from_the_kernel_layout() {
        let mut b = raw(10, 500_000, EV_KEY, KEY_VOLUMEDOWN, 1);
        b.extend(raw(12, 600_000, 0, 0, 0)); // EV_SYN
        b.extend([1, 2, 3]); // a partial event is ignored
        assert_eq!(
            parse(&b),
            vec![(EV_KEY, KEY_VOLUMEDOWN, 1, 10_500), (0, 0, 0, 12_600)]
        );
    }

    #[test]
    fn only_a_long_enough_hold_of_the_key_counts() {
        let mut h = Hold::new(KEY_VOLUMEDOWN, 2000);
        // a short press: the volume's, not ours
        assert!(!h.feed((EV_KEY, KEY_VOLUMEDOWN, 1, 1000)));
        assert!(!h.feed((EV_KEY, KEY_VOLUMEDOWN, 0, 1300)));
        // a hold, with autorepeat and another key in between
        assert!(!h.feed((EV_KEY, KEY_VOLUMEDOWN, 1, 5000)));
        assert!(!h.feed((EV_KEY, KEY_VOLUMEDOWN, 2, 5500)));
        assert!(!h.feed((EV_KEY, 115, 1, 6000)));
        assert!(h.feed((EV_KEY, KEY_VOLUMEDOWN, 0, 7100)));
        // a release without a press (agentd started mid-hold) does nothing
        assert!(!h.feed((EV_KEY, KEY_VOLUMEDOWN, 0, 9000)));
    }
}
