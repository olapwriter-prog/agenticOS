//! Inbox triage: for each conversation's newest message, a group (Reply, Bills, Updates,
//! Promotions, People), what to do with it (reply, pay, delete or keep), why, and a draft reply.
//! Rules, so it is instant and private; the owner sees the reason and decides. Nothing is sent or
//! deleted by triage itself.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Triage {
    /// "Reply", "Bills", "Updates", "Promotions" or "People"
    pub group: String,
    /// "reply", "pay", "delete" or "keep"
    pub suggest: String,
    pub reason: String,
    /// a reply the owner can send as it is, or edit ("" when none fits)
    pub draft: String,
}

fn has(t: &str, words: &[&str]) -> bool {
    words.iter().any(|w| t.contains(w))
}

/// `known` is true when the sender is in the contacts (or a conversation the owner has replied in).
pub fn triage(text: &str, known: bool) -> Triage {
    let t = format!(" {} ", text.to_lowercase());
    let mk = |group: &str, suggest: &str, reason: &str, draft: &str| Triage {
        group: group.into(),
        suggest: suggest.into(),
        reason: reason.into(),
        draft: draft.into(),
    };
    // promotions and spam first: they often contain questions too ("want 50% off?")
    if has(
        &t,
        &[
            " offer",
            "% off",
            " sale ",
            "cashback",
            "discount",
            " win ",
            "winner",
            "lottery",
            "click here",
            "unsubscribe",
            "limited time",
            "free gift",
            "loan approved",
            "pre-approved",
        ],
    ) {
        return mk("Promotions", "delete", "Looks like an offer or spam", "");
    }
    if has(
        &t,
        &[
            " otp",
            "one time password",
            "verification code",
            " code is ",
        ],
    ) {
        return mk(
            "Updates",
            "delete",
            "A one-time code; not needed after use",
            "",
        );
    }
    if has(
        &t,
        &[
            "invoice", " bill ", " due ", "payment", "pay by", "amount", "₹", " rs ", " inr ",
        ],
    ) {
        return mk(
            "Bills",
            "pay",
            "Money is due",
            if known { "Got it, paying today" } else { "" },
        );
    }
    if has(
        &t,
        &[
            "delivered",
            "shipped",
            "out for delivery",
            "order ",
            "booking confirmed",
            "your ride",
            "statement",
        ],
    ) {
        return mk("Updates", "keep", "A status update; nothing to do", "");
    }
    let question = t.contains('?')
        || has(
            &t,
            &[
                " can you",
                " could you",
                " will you",
                " are you",
                " call me",
                " let me know",
                " please ",
            ],
        );
    if question {
        let draft = if has(&t, &["late", "where are you", "coming", "reach", "eta"]) {
            "On my way, 10 minutes"
        } else if has(&t, &["call me", "call when", "free to talk"]) {
            "Will call you in 10 minutes"
        } else if has(
            &t,
            &["tonight", "tomorrow", "match", "dinner", "lunch", "meet"],
        ) {
            "Sounds good, see you then"
        } else {
            "Yes, will do"
        };
        return mk(
            "Reply",
            "reply",
            if known {
                "A question from someone you know"
            } else {
                "A question from an unknown sender"
            },
            if known { draft } else { "" },
        );
    }
    if known {
        mk("People", "keep", "From someone you know", "")
    } else {
        mk("Updates", "keep", "No reply needed", "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_from_people_want_a_reply_with_a_draft() {
        let t = triage("Running 10 min late?", true);
        assert_eq!((t.group.as_str(), t.suggest.as_str()), ("Reply", "reply"));
        assert_eq!(t.draft, "On my way, 10 minutes");
        assert_eq!(
            triage("Call me when free", true).draft,
            "Will call you in 10 minutes"
        );
        assert_eq!(
            triage("Match tonight at 8?", true).draft,
            "Sounds good, see you then"
        );
        // an unknown sender gets no ready-made answer
        assert_eq!(triage("are you there?", false).draft, "");
    }

    #[test]
    fn offers_and_codes_can_go() {
        assert_eq!(
            triage("Mega SALE! 50% off today only", false).suggest,
            "delete"
        );
        assert_eq!(triage("Your OTP is 482913", false).suggest, "delete");
        assert_eq!(
            triage("Want a pre-approved loan? Click here", false).group,
            "Promotions"
        );
    }

    #[test]
    fn bills_updates_and_people() {
        let b = triage("Here is the invoice, ₹4,720 due 20 Nov", true);
        assert_eq!((b.group.as_str(), b.suggest.as_str()), ("Bills", "pay"));
        assert_eq!(
            triage("Your order has been shipped", false).group,
            "Updates"
        );
        assert_eq!(triage("Reached home", true).group, "People");
    }
}
