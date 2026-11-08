//! Signed updates and ledger anchors.
//!
//! `manifest`: an update is a payload plus a manifest signed with an ed25519 key. The phone holds
//! only the public key. It refuses a bad signature, a wrong hash or size, and any version that is
//! not newer than the one installed (anti-rollback).
//!
//! `anchor`: a keyed checkpoint over the ledger head, closing the Vol 2 open item. Someone who can
//! rewrite the whole ledger still cannot forge a matching anchor without the anchor key.
pub mod anchor;
pub mod manifest;
