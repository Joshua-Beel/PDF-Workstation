## 0.3.0

The first update since 0.2.0. Everything below was tested in source builds. These notes were written before the 0.3.0 installer was built, so each item is unverified in an installed 0.3.0 build.

Reading and search
- Find text with Ctrl+F: case matching, one excerpt per matching page (up to 500 pages), and highlights on the page where the PDF gives text positions.
- Select embedded text on the page, or open it in the Page text view, and copy it.
- Page labels, nested bookmarks, document properties (Ctrl+D), and opening password-protected PDFs. Passwords are not saved, and protected PDFs open read-only.
- Print all pages, the current page or a range with Ctrl+P, including your current edits.

OCR, on your computer
- The installer includes a local English OCR engine (Tesseract). Recognize the current page into a copyable text window, or save a searchable OCR copy of a document of up to 32 pages: a new PDF in which each page becomes a 150-DPI image with an invisible text layer. Nothing is uploaded. Accuracy on real scans is unmeasured.

Pages
- Split, crop and reset crop, move to page, insert pages from another PDF, replace pages, and combine two PDFs into a new copy.
- Create a PDF from 1 to 32 PNG or JPEG images, and export the current page as a PNG or JPEG image.
- Raster redaction: black out rectangles on one page and save a new copy in which every page becomes a 150-DPI image. Text, forms, annotations and bookmarks are removed from that copy on purpose.

Comments and forms
- Sticky notes, area highlights and highlights on selected text.
- Fill a limited set of existing form fields (single-line text, including accented Latin letters such as é and ñ; checkboxes; radio buttons; single-choice lists, including scrolling list boxes) into a new copy. Forms outside that set are refused rather than partly filled.

Workspace and updates
- Remembers theme, zoom, panels and up to 50 recent files, with stars.
- New in 0.3.0, for future updates: before an update installs, the app asks you to confirm and to save your work first. Choosing No keeps your current version.
- PDF Workstation is now MIT-licensed. Third-party notices are listed in the app.

Still not included: text or image editing, saving over the original file (every save writes a new copy), PDF signing, certificate signatures or signature validation, form preparation, and redaction that keeps text searchable. Printers other than Microsoft Print to PDF are untested.
