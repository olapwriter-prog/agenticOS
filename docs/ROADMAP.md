# Roadmap: from a phone that acts to a partner that works

What exists today (phases 1–7, see PROGRESS.md): a headless phone run by one daemon, with the
network's events, calls and texts, contacts, the phone's switches, a calendar and reminders, a
sandbox that answers questions over the phone's own data with code, an agent that writes its own
functions in Rust, a tiered approval model with a hardware key, a hash-chained ledger, its own
inference engine and trainer, and overnight training on what the human confirmed.

What is asked for next is a different kind of thing: the phone as a **business partner and a
worker**. Below, each capability is scoped with what it needs. None of it is built yet; the
order is by what the existing pieces already support.

## The one design rule that carries over

Everything high-risk (money, messages to other people, filings) stays behind the approval key,
is recorded in the ledger, and is learned from. The agent prepares; the human presses.

## Phase 8: channels — WhatsApp, email, SMS as one inbox

- **WhatsApp Business**: Meta's Cloud API (a verified business, a phone number, an access token;
  messages cost money per conversation). A `whatsapp` connector like the SMS one: read
  conversations, draft replies, send after the key. Personal WhatsApp has no API; only the
  business product is reachable without breaking Meta's terms.
- **Email** (IMAP/SMTP) and the existing SMS: the same connector shape.
- **Triage by the local model**: customer, supplier, spam, urgent; a reply drafted per thread.
  The sandbox's data path keeps message bodies away from any commercial model unless the human
  allows it per thread.
- Needs: credentials in the vault, a network policy (which hosts the daemon may reach), tests on
  recorded conversations.

## Phase 9: money in — invoices, GST

- **Invoices**: a local ledger of customers, items, GST rates; invoices as PDFs from a template;
  sent over a channel after the key. All local, no external service.
- **GST filing** (India): the GSTN has no public filing API; filing goes through a licensed GSP
  (GST Suvidha Provider) with a paid API, or through the portal by hand. Realistic scope: the
  agent prepares GSTR-1/3B figures from the invoice ledger, reconciles with purchase bills, and
  hands the human a filled return to upload; with a GSP account, it files after the key.
- **E-invoicing / e-way bills**: the same GSP route.
- Needs: a GSP agreement (the business's, not the OS's), GSTIN credentials in the vault.

## Phase 10: money out — paying expenses, wallets, shopping

- **Payments**: UPI is the way on an Indian phone. There is no open UPI API for an app that is
  not a licensed payment app; the practical routes are (a) UPI deep links / intents handed to an
  installed payment app, with the human completing in that app, or (b) a payment-aggregator
  business account (Razorpay, Cashfree, …) for payouts, with its API key in the vault. Every
  payment is a High action: the key, the ledger, a daily budget already in the config.
- **Bills**: due-date tracking from SMS/email (the data path already parses texts), reminders,
  prepared payments.
- **Shopping with the order history**: an `orders` store fed from order emails/SMS; a price check
  across platforms is web scraping or each platform's affiliate API; the agent presents the one
  best option (UI, not a list) and buys only after the key. Needs network policy and per-site
  parsers that will break often: scope it to a few platforms.

## Phase 11: personal life — news, interests, bookings

- **Interests** learned from what the human reads, asks and confirms (the learner already keeps
  episodes; add topics).
- **News**: RSS/Atom feeds pulled on the charger; the local model ranks and summarises against
  the interests; nothing leaves the phone.
- **Bookings** (events, films, music, travel): each is a platform API (BookMyShow and the like
  have partner APIs, not public ones) or a web flow. The honest scope: discover and compare
  locally, then hand the human a prepared booking page or a deep link; pay after the key.

## Phase 12: UIs instead of text

The screen exists (agentos-ui, Rune) and already renders agentd's `state` JSON. What is asked
for is the agent **composing a screen per request**: an invoice to approve, three options to
choose from, a thread to reply to. The path that fits this OS:

- The agent answers with a small declarative UI document (cards, lists, a form, one or two
  buttons), not pixels and not code. A fixed set of widgets keeps it safe and fast to render.
- agentos-ui renders it; buttons map to the same tiered actions (a "Pay" button is the key).
- The local model writes the document under a grammar, the way it already writes tool calls
  under one; the sandbox fills in data. The grammar makes a bad screen impossible.
- Headless use stays: without a screen, the same document is read out as text or over SMS.

## Phase 13: the model tunes to the person, daily

Overnight training exists. What is missing for "daily, as per the user":
- more signals than approvals (which option they chose, what they corrected, what they read),
- per-area adapters (channels, money, personal) swapped by context,
- the eval gate on the person's own held-out requests, so a worse day is discarded,
- a budget: training only on the charger, only when cool, only so many minutes.

## What to decide first

1. Which channel (WhatsApp Business needs a business account before any code is useful).
2. Whether a GSP and a payment aggregator account exist or will.
3. Which two or three shopping/booking platforms matter.
4. Whether the screen is required for the first release, or text/SMS answers are enough.
