"""Fixtures for agentos-docs, made by the usual producers (python-docx, openpyxl, python-pptx,
reportlab, and LibreOffice for recalculated formulas and a second PDF producer).
    python3 crates/docs/tests/make_fixtures.py crates/docs/tests/fixtures
"""
import os, subprocess, sys, shutil, tempfile
from docx import Document
from openpyxl import Workbook
from pptx import Presentation
from reportlab.lib.pagesizes import A4
from reportlab.pdfgen import canvas

out = sys.argv[1]
os.makedirs(out, exist_ok=True)

d = Document()
d.add_heading("Service Agreement", 0)
d.add_paragraph("This agreement is between Lakshmi Widgets and Patil Stores.")
d.add_heading("1. Payment terms", 1)
d.add_paragraph("Invoices are payable within 30 days of the invoice date. Late payments carry 1.5% interest per month.")
d.add_heading("2. Delivery", 1)
d.add_paragraph("Goods are delivered to Pune within 7 working days.")
t = d.add_table(rows=3, cols=3)
for r, row in enumerate([["Item", "Qty", "Price"], ["Widget", "10", "500"], ["Repair", "2", "1500"]]):
    for c, v in enumerate(row):
        t.cell(r, c).text = v
d.add_paragraph("Signed in Bengaluru.")
d.save(f"{out}/agreement.docx")

wb = Workbook()
ws = wb.active
ws.title = "Sales"
ws.append(["Month", "Units", "Revenue"])
for m, u, r in [("Sep", 120, 60000), ("Oct", 150, 75000), ("Nov", 90, 45000)]:
    ws.append([m, u, r])
ws.append(["Total", "=SUM(B2:B4)", "=SUM(C2:C4)"])
ws2 = wb.create_sheet("Expenses")
ws2.append(["Supplier", "Amount", "Paid"])
ws2.append(["City Power", 4720.5, True])
ws2.append(["FiberNet", 1179, False])
tmp = tempfile.mkdtemp()
wb.save(f"{tmp}/sales.xlsx")
# LibreOffice recalculates the formulas and stores their values, as Excel would
subprocess.run(["soffice", "--headless", "--convert-to", "xlsx", "--outdir", out, f"{tmp}/sales.xlsx"], check=True, capture_output=True)

p = Presentation()
s = p.slides.add_slide(p.slide_layouts[0])
s.shapes.title.text = "Quarterly review"
s.placeholders[1].text = "Lakshmi Widgets, Q3"
s = p.slides.add_slide(p.slide_layouts[1])
s.shapes.title.text = "Revenue"
s.placeholders[1].text = "Revenue grew 25% in October\nNovember dipped with the festival season"
s = p.slides.add_slide(p.slide_layouts[1])
s.shapes.title.text = "Next steps"
s.placeholders[1].text = "Open a store in Mysuru\nHire two technicians"
p.save(f"{out}/review.pptx")

c = canvas.Canvas(f"{out}/statement.pdf", pagesize=A4)
c.drawString(72, 800, "Bank statement, November 2026")
c.drawString(72, 780, "Opening balance: 1,50,000.00")
c.drawString(72, 760, "UPI to fibernet@upi 1,179.00")
c.showPage()
c.drawString(72, 800, "Page two: closing balance 1,48,821.00")
c.save()
subprocess.run(["soffice", "--headless", "--convert-to", "pdf", "--outdir", tmp, f"{out}/agreement.docx"], check=True, capture_output=True)
shutil.copy(f"{tmp}/agreement.pdf", f"{out}/agreement.pdf")
for f in sorted(os.listdir(out)):
    print(f, os.path.getsize(f"{out}/{f}"))
