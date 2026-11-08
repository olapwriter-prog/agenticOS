//! What the phone hardware can do, as one trait. `Sim` keeps everything in memory for tests and
//! for running the daemon on a laptop; `crate::linux::Linux` drives a real phone through
//! ModemManager, NetworkManager, feedbackd and sysfs.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    /// Someone is calling us.
    Ringing,
    /// We are calling someone and they have not answered.
    Dialing,
    Active,
    Held,
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub id: String,
    pub number: String,
    pub incoming: bool,
    pub state: CallState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sms {
    pub id: String,
    pub number: String,
    pub text: String,
    pub incoming: bool,
    pub time: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetStatus {
    pub operator: String,
    /// 0 to 100.
    pub signal: u8,
    /// lte, umts, gsm, 5gnr...
    pub tech: String,
    pub registered: bool,
    pub wifi: Option<String>,
    pub mobile_data: bool,
    pub airplane: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiNet {
    pub ssid: String,
    pub signal: u8,
}

pub trait Backend {
    fn status(&mut self) -> Result<NetStatus, String>;
    /// Start a call; returns its id.
    fn dial(&mut self, number: &str) -> Result<String, String>;
    fn answer(&mut self, id: &str) -> Result<(), String>;
    fn hangup(&mut self, id: &str) -> Result<(), String>;
    fn calls(&mut self) -> Result<Vec<Call>, String>;
    fn send_sms(&mut self, number: &str, text: &str) -> Result<(), String>;
    fn messages(&mut self) -> Result<Vec<Sms>, String>;
    /// Send a USSD code such as *123# and return the network's reply.
    fn ussd(&mut self, code: &str) -> Result<String, String>;
    fn wifi_scan(&mut self) -> Result<Vec<WifiNet>, String>;
    fn wifi_connect(&mut self, ssid: &str, password: &str) -> Result<(), String>;
    fn set_airplane(&mut self, on: bool) -> Result<(), String>;
    fn set_mobile_data(&mut self, on: bool) -> Result<(), String>;
    /// A human-readable position: GPS coordinates if there is a fix, else the serving cell.
    fn location(&mut self) -> Result<String, String>;
    fn torch(&mut self, on: bool) -> Result<(), String>;
    fn set_volume(&mut self, percent: u8) -> Result<(), String>;
    /// Vibrate / play a sound for a feedbackd event name such as "message-new-sms".
    fn feedback(&mut self, event: &str) -> Result<(), String>;

    /// Simulator only: pretend the network delivered a text or a call.
    fn inject_sms(&mut self, _from: &str, _text: &str) -> Result<(), String> {
        Err("only the simulated phone can do that".into())
    }
    fn inject_call(&mut self, _from: &str) -> Result<(), String> {
        Err("only the simulated phone can do that".into())
    }
}

/// An in-memory phone. Everything it was asked to do is visible in its fields.
#[derive(Debug, Default)]
pub struct Sim {
    pub calls: Vec<Call>,
    pub sms: Vec<Sms>,
    pub wifi: Option<String>,
    pub airplane: bool,
    pub mobile_data: bool,
    pub torch: bool,
    pub volume: u8,
    pub feedback: Vec<String>,
    next_id: u64,
}

impl Sim {
    pub fn new() -> Self {
        Sim {
            mobile_data: true,
            volume: 60,
            ..Default::default()
        }
    }
    fn id(&mut self) -> String {
        self.next_id += 1;
        self.next_id.to_string()
    }
    fn radio_ok(&self) -> Result<(), String> {
        if self.airplane {
            Err("airplane mode is on".into())
        } else {
            Ok(())
        }
    }
    fn call_mut(&mut self, id: &str) -> Result<&mut Call, String> {
        self.calls
            .iter_mut()
            .find(|c| c.id == id && c.state != CallState::Ended)
            .ok_or_else(|| format!("no call {id}"))
    }
}

impl Backend for Sim {
    fn status(&mut self) -> Result<NetStatus, String> {
        Ok(NetStatus {
            operator: if self.airplane {
                String::new()
            } else {
                "SimNet".into()
            },
            signal: if self.airplane { 0 } else { 72 },
            tech: if self.airplane {
                String::new()
            } else {
                "lte".into()
            },
            registered: !self.airplane,
            wifi: self.wifi.clone(),
            mobile_data: self.mobile_data && !self.airplane,
            airplane: self.airplane,
        })
    }
    fn dial(&mut self, number: &str) -> Result<String, String> {
        self.radio_ok()?;
        let id = self.id();
        self.calls.push(Call {
            id: id.clone(),
            number: number.into(),
            incoming: false,
            state: CallState::Dialing,
        });
        Ok(id)
    }
    fn answer(&mut self, id: &str) -> Result<(), String> {
        let c = self.call_mut(id)?;
        if c.state != CallState::Ringing {
            return Err("that call is not ringing".into());
        }
        c.state = CallState::Active;
        Ok(())
    }
    fn hangup(&mut self, id: &str) -> Result<(), String> {
        self.call_mut(id)?.state = CallState::Ended;
        Ok(())
    }
    fn calls(&mut self) -> Result<Vec<Call>, String> {
        Ok(self.calls.clone())
    }
    fn send_sms(&mut self, number: &str, text: &str) -> Result<(), String> {
        self.radio_ok()?;
        let id = self.id();
        self.sms.push(Sms {
            id,
            number: number.into(),
            text: text.into(),
            incoming: false,
            time: String::new(),
        });
        Ok(())
    }
    fn messages(&mut self) -> Result<Vec<Sms>, String> {
        Ok(self.sms.clone())
    }
    fn ussd(&mut self, code: &str) -> Result<String, String> {
        self.radio_ok()?;
        Ok(format!("SimNet: reply to {code}: balance 100.00"))
    }
    fn wifi_scan(&mut self) -> Result<Vec<WifiNet>, String> {
        if self.airplane {
            return Ok(vec![]);
        }
        Ok(vec![
            WifiNet {
                ssid: "HomeNet".into(),
                signal: 80,
            },
            WifiNet {
                ssid: "Cafe".into(),
                signal: 45,
            },
        ])
    }
    fn wifi_connect(&mut self, ssid: &str, _password: &str) -> Result<(), String> {
        if !self.wifi_scan()?.iter().any(|n| n.ssid == ssid) {
            return Err(format!("no network called {ssid}"));
        }
        self.wifi = Some(ssid.into());
        Ok(())
    }
    fn set_airplane(&mut self, on: bool) -> Result<(), String> {
        self.airplane = on;
        if on {
            self.wifi = None;
            for c in &mut self.calls {
                c.state = CallState::Ended;
            }
        }
        Ok(())
    }
    fn set_mobile_data(&mut self, on: bool) -> Result<(), String> {
        self.mobile_data = on;
        Ok(())
    }
    fn location(&mut self) -> Result<String, String> {
        Ok("12.971600, 77.594600 (simulated)".into())
    }
    fn torch(&mut self, on: bool) -> Result<(), String> {
        self.torch = on;
        Ok(())
    }
    fn set_volume(&mut self, percent: u8) -> Result<(), String> {
        self.volume = percent;
        Ok(())
    }
    fn feedback(&mut self, event: &str) -> Result<(), String> {
        self.feedback.push(event.into());
        Ok(())
    }
    fn inject_sms(&mut self, from: &str, text: &str) -> Result<(), String> {
        let id = self.id();
        self.sms.push(Sms {
            id,
            number: from.into(),
            text: text.into(),
            incoming: true,
            time: String::new(),
        });
        Ok(())
    }
    fn inject_call(&mut self, from: &str) -> Result<(), String> {
        let id = self.id();
        self.calls.push(Call {
            id,
            number: from.into(),
            incoming: true,
            state: CallState::Ringing,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn airplane_mode_ends_calls_and_blocks_radio() {
        let mut s = Sim::new();
        let id = s.dial("+911234567890").unwrap();
        s.set_airplane(true).unwrap();
        assert_eq!(s.calls[0].state, CallState::Ended);
        assert!(s.send_sms("+911234567890", "hi").is_err());
        assert!(s.hangup(&id).is_err());
    }
    #[test]
    fn only_ringing_calls_can_be_answered() {
        let mut s = Sim::new();
        let out = s.dial("+911234567890").unwrap();
        assert!(s.answer(&out).is_err());
        s.inject_call("+919999999999").unwrap();
        let ring = s.calls[1].id.clone();
        s.answer(&ring).unwrap();
        assert_eq!(s.calls[1].state, CallState::Active);
    }
}
