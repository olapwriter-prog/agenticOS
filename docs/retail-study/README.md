# A 5,000-bills-a-day store audited on the phone: the fiscal-year study

`crates/retail` lets agentd keep a retail store's books from a profile file
(`examples/retail/profile.toml`). Each day's bills from the tills are audited line by line by
rules (no model): the GST rate against HSN, price slab and date; the arithmetic; totals and
payment; the invoice series; business buyers' GSTINs and place of supply; returns against their
original bill; the tills' cash. The phone keeps each day's totals and exceptions, sealed. It does
not keep the bills, and a shopper's phone number is dropped as the bill is read.

From the days come the month's GST return (output tax by rate and HSN; input tax credit only for
invoices the supplier reported, once, and not on blocked items), the year's tally, and the
income-tax computation. Tools: `shop_audit`, `shop_exceptions`, `shop_gst`, `shop_year` (read),
`shop_propose_fix` (anyone connected may propose; nothing changes), and `shop_apply_fixes`,
`shop_file_gst`, `shop_file_income_tax` (approval key).

Run the study (about ten seconds):

```bash
cargo build --release -p agentd
```
```bash
python3 scripts/mac/store-sim.py
```

It invents FY 2025-26 for the store with faults planted at known places, scores the audit
against them month by month, has a client propose fixes over MCP, and has the owner apply them
and file with the key. Results: `target/store-sim/results.json`; the year as the owner sees it:
`year-tally.txt` here (run of 13 Nov 2026).

What is simulated: the store, its shoppers, suppliers and bills; the GST and income-tax portals.
Every rate, date and amount in the profile is an assumption to check with an accountant: the
clothing and footwear slabs before and after 22 Sep 2025, the block on credit for fit-out, the
company tax rate, and the expense levels.
