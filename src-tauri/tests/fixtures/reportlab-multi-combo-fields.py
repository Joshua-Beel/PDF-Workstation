"""Independent test-only fixture generated with ReportLab 4.4.9; no CI Python requirement."""
from pathlib import Path
from reportlab.pdfgen import canvas
from reportlab.lib.colors import Color, black, blue

path = Path(__file__).with_suffix('.pdf')
c = canvas.Canvas(str(path), pagesize=(612, 792), invariant=1)
c.drawString(60, 740, 'Independent multi-select list and editable combo fields')
common = dict(fontName='Helvetica', fontSize=12, borderWidth=1, borderStyle='solid', borderColor=blue, textColor=black, fillColor=Color(.92, .96, 1), annotationFlags='print')
c.drawString(60, 704, 'Services: multi-select list, one selected')
c.acroForm.listbox(name='Services', x=60, y=584, width=220, height=100, options=[('Pickup', 'pick-001'), ('Delivery', 'deliv-002'), ('Storage', 'store-003'), ('Insurance', 'insur-004')], value='pick-001', fieldFlags='multiSelect', **common)
c.drawString(60, 544, 'Carrier: editable combo, listed option selected')
c.acroForm.choice(name='Carrier', x=60, y=504, width=220, height=28, options=[('Atlas Freight', 'atlas-001'), ('Birch Courier', 'birch-002')], value='atlas-001', fieldFlags='combo edit', **common)
c.drawString(60, 464, 'Priority: editable combo holding a listed option')
c.acroForm.choice(name='Priority', x=60, y=424, width=220, height=28, options=[('Standard', 'std-001'), ('Rush', 'rush-002')], value='std-001', fieldFlags='combo edit', **common)
c.showPage()
c.save()
